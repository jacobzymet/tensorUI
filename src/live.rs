//! Detached chat turns: generation keeps running after a browser refresh,
//! and any tab can subscribe to the same conversation's live SSE.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures_util::StreamExt;
use serde::Serialize;
use tokio::sync::{broadcast, watch};

use crate::agent::chat::{ChatStream, sse_error};

const BROADCAST_CAP: usize = 256;
const MAX_BUFFERED_BYTES: u64 = 32 * 1024 * 1024;
const MAX_BUFFERED_FRAMES: usize = 100_000;
const CANCEL_TOMBSTONE_TTL: Duration = Duration::from_secs(60);
const CANCEL_SETTLE_TIMEOUT: Duration = Duration::from_secs(5);
const PUMP_ABORT_TIMEOUT: Duration = Duration::from_millis(500);
#[cfg(not(test))]
const LINGER: Duration = Duration::from_secs(90);
#[cfg(test)]
const LINGER: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Serialize)]
pub struct LiveTurnInfo {
    pub conversation_id: String,
    pub turn_id: String,
    pub agent: bool,
    pub deep_research: bool,
    pub deep_research_output: String,
    pub model: String,
    pub finished: bool,
}

#[derive(Clone)]
struct StoredFrame {
    seq: u64,
    bytes: Vec<u8>,
}

struct LiveTurn {
    info: LiveTurnInfo,
    frames: Mutex<Vec<StoredFrame>>,
    buffered_bytes: AtomicU64,
    seq: AtomicU64,
    tx: broadcast::Sender<StoredFrame>,
    cancel: watch::Sender<bool>,
    done: watch::Sender<bool>,
    finished: AtomicBool,
    pump: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl LiveTurn {
    fn new(mut info: LiveTurnInfo) -> Self {
        info.finished = false;
        let (tx, _) = broadcast::channel(BROADCAST_CAP);
        let (cancel, _) = watch::channel(false);
        let (done, _) = watch::channel(false);
        Self {
            info,
            frames: Mutex::new(Vec::new()),
            buffered_bytes: AtomicU64::new(0),
            seq: AtomicU64::new(0),
            tx,
            cancel,
            done,
            finished: AtomicBool::new(false),
            pump: Mutex::new(None),
        }
    }

    fn snapshot_info(&self) -> LiveTurnInfo {
        let mut info = self.info.clone();
        info.finished = self.finished.load(Ordering::SeqCst);
        info
    }

    fn push_bytes(&self, bytes: Vec<u8>) -> bool {
        if bytes.is_empty() {
            return true;
        }
        let byte_len = bytes.len() as u64;
        let previous = self.buffered_bytes.fetch_add(byte_len, Ordering::SeqCst);
        if !within_replay_limit(previous, 0, byte_len) {
            self.buffered_bytes.fetch_sub(byte_len, Ordering::SeqCst);
            return false;
        }
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let frame = StoredFrame { seq, bytes };
        let stored = if let Ok(mut frames) = self.frames.lock() {
            if !within_replay_limit(previous, frames.len(), byte_len) {
                false
            } else {
                frames.push(frame.clone());
                true
            }
        } else {
            false
        };
        if !stored {
            self.buffered_bytes.fetch_sub(byte_len, Ordering::SeqCst);
            return false;
        }
        let _ = self.tx.send(frame);
        true
    }

    fn push_terminal_bytes(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let frame = StoredFrame { seq, bytes };
        if let Ok(mut frames) = self.frames.lock() {
            frames.push(frame.clone());
        }
        let _ = self.tx.send(frame);
    }

    fn finish(&self) {
        self.finished.store(true, Ordering::SeqCst);
        let _ = self.done.send(true);
    }

    fn request_cancel(&self) {
        self.cancel.send_replace(true);
    }

    fn cancel_requested(&self) -> bool {
        *self.cancel.borrow()
    }

    fn mark_cancelled_done(&self) {
        self.push_terminal_bytes(b"data: [DONE]\n\n".to_vec());
        self.finish();
    }

    fn set_pump(&self, handle: tokio::task::JoinHandle<()>) {
        match self.pump.lock() {
            Ok(mut slot) => *slot = Some(handle),
            Err(_) => handle.abort(),
        }
    }

    fn take_pump(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.pump.lock().ok()?.take()
    }

    async fn wait_done(&self) -> bool {
        if self.finished.load(Ordering::SeqCst) {
            return true;
        }
        let mut done = self.done.subscribe();
        if *done.borrow() {
            return true;
        }
        while done.changed().await.is_ok() {
            if *done.borrow() {
                return true;
            }
        }
        self.finished.load(Ordering::SeqCst)
    }

    fn meta_frame(&self) -> Vec<u8> {
        let payload = serde_json::to_string(&self.snapshot_info()).unwrap_or_else(|_| "{}".into());
        format!("event: meta\ndata: {payload}\n\n").into_bytes()
    }

    fn replay_after(&self, last_seq: u64) -> (Vec<StoredFrame>, u64) {
        let extra = self
            .frames
            .lock()
            .map(|frames| {
                let start = frames.partition_point(|frame| frame.seq <= last_seq);
                frames[start..].to_vec()
            })
            .unwrap_or_default();
        let next = extra.last().map_or(last_seq, |frame| frame.seq);
        (extra, next)
    }

    fn subscribe_stream(self: Arc<Self>) -> ChatStream {
        Box::pin(async_stream::stream! {
            yield Ok(self.meta_frame());
            let mut rx = self.tx.subscribe();
            let mut done_rx = self.done.subscribe();
            let snapshot = self
                .frames
                .lock()
                .map(|frames| frames.clone())
                .unwrap_or_default();
            let mut last_seq = 0;
            for frame in snapshot {
                last_seq = frame.seq;
                yield Ok(frame.bytes);
            }
            yield Ok(b"event: live\ndata: {}\n\n".to_vec());
            if self.finished.load(Ordering::SeqCst) {
                return;
            }
            loop {
                tokio::select! {
                    recv = rx.recv() => {
                        match recv {
                            Ok(frame) => {
                                if frame.seq <= last_seq {
                                    continue;
                                }
                                last_seq = frame.seq;
                                yield Ok(frame.bytes);
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => {
                                let (extra, next) = self.replay_after(last_seq);
                                last_seq = next;
                                for frame in extra {
                                    yield Ok(frame.bytes);
                                }
                                if self.finished.load(Ordering::SeqCst) {
                                    return;
                                }
                            }
                            Err(broadcast::error::RecvError::Closed) => return,
                        }
                    }
                    _ = done_rx.changed() => {
                        if !*done_rx.borrow() {
                            continue;
                        }
                        let (extra, _) = self.replay_after(last_seq);
                        for frame in extra {
                            yield Ok(frame.bytes);
                        }
                        return;
                    }
                }
            }
        })
    }
}

fn within_replay_limit(current_bytes: u64, current_frames: usize, incoming_bytes: u64) -> bool {
    current_frames < MAX_BUFFERED_FRAMES
        && current_bytes.saturating_add(incoming_bytes) <= MAX_BUFFERED_BYTES
}

#[derive(Clone)]
pub struct LiveHub {
    turns: Arc<Mutex<HashMap<String, Arc<LiveTurn>>>>,
    cancelled_turns: Arc<Mutex<HashMap<(String, String), Instant>>>,
}

impl LiveHub {
    fn new() -> Self {
        Self {
            turns: Arc::new(Mutex::new(HashMap::new())),
            cancelled_turns: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn lock_turns(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<LiveTurn>>> {
        self.turns.lock().unwrap_or_else(|err| err.into_inner())
    }

    fn lock_cancelled(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), Instant>> {
        self.cancelled_turns
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    fn prune_cancelled(cancelled: &mut HashMap<(String, String), Instant>) {
        let now = Instant::now();
        cancelled.retain(|_, at| now.saturating_duration_since(*at) < CANCEL_TOMBSTONE_TTL);
    }

    fn cancellation_target(
        &self,
        conversation_id: &str,
        turn_id: Option<&str>,
    ) -> Option<Arc<LiveTurn>> {
        // Always lock turns before cancelled_turns so registration and
        // cancel-before-start are atomic with each other.
        let turns = self.lock_turns();
        if let Some(turn) = turns.get(conversation_id).cloned()
            && turn_id.is_none_or(|expected| turn.snapshot_info().turn_id == expected)
        {
            return Some(turn);
        }
        if let Some(turn_id) = turn_id {
            let mut cancelled = self.lock_cancelled();
            Self::prune_cancelled(&mut cancelled);
            cancelled.insert(
                (conversation_id.to_string(), turn_id.to_string()),
                Instant::now(),
            );
        }
        None
    }

    pub fn list(&self) -> Vec<LiveTurnInfo> {
        self.lock_turns()
            .values()
            .map(|turn| turn.snapshot_info())
            .collect()
    }

    pub fn subscribe(&self, conversation_id: &str) -> Option<ChatStream> {
        let turn = self.lock_turns().get(conversation_id).cloned()?;
        Some(turn.subscribe_stream())
    }

    pub fn clear(&self) {
        let turns: Vec<_> = self.lock_turns().drain().map(|(_, turn)| turn).collect();
        self.lock_cancelled().clear();
        for turn in turns {
            turn.request_cancel();
            turn.frames
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clear();
        }
    }

    pub fn info(&self, conversation_id: &str) -> Option<LiveTurnInfo> {
        self.lock_turns()
            .get(conversation_id)
            .map(|turn| turn.snapshot_info())
    }

    pub fn cancel(&self, conversation_id: &str, turn_id: Option<&str>) -> bool {
        let Some(turn) = self.cancellation_target(conversation_id, turn_id) else {
            return false;
        };
        if turn.finished.load(Ordering::SeqCst) {
            return false;
        }
        turn.request_cancel();
        true
    }

    pub async fn cancel_and_wait(
        &self,
        conversation_id: &str,
        turn_id: Option<&str>,
    ) -> (bool, bool) {
        let Some(turn) = self.cancellation_target(conversation_id, turn_id) else {
            // An exact-id cancellation that arrived before registration is now
            // tombstoned, so that late request can no longer start.
            return (false, true);
        };
        if turn.finished.load(Ordering::SeqCst) {
            self.drop_if_same(&turn);
            return (false, true);
        }

        turn.request_cancel();
        let finished = tokio::time::timeout(CANCEL_SETTLE_TIMEOUT, turn.wait_done())
            .await
            .unwrap_or(false);

        if !finished {
            // Graceful cancellation should be immediate. Hard-abort the pump
            // as a fail-safe so a wedged upstream cannot occupy this slot.
            if let Some(mut pump) = turn.take_pump() {
                pump.abort();
                let _ = tokio::time::timeout(PUMP_ABORT_TIMEOUT, &mut pump).await;
            }
            if !turn.finished.load(Ordering::SeqCst) {
                turn.mark_cancelled_done();
            }
        }
        // Always free the conversation. Linger is only for finished replay;
        // a cancelled turn must never block the next prompt until restart.
        self.drop_if_same(&turn);
        (true, true)
    }

    pub fn start(
        &self,
        info: LiveTurnInfo,
        source: ChatStream,
    ) -> Result<ChatStream, LiveTurnInfo> {
        let lease = crate::session::lease().map_err(|_| info.clone())?;
        let conversation_id = info.conversation_id.clone();
        let turn_id = info.turn_id.clone();
        let mut displaced = None;
        let mut displaced_turn = None;
        let turn = {
            let mut turns = self.lock_turns();
            if !lease.valid() {
                return Err(info);
            }
            {
                let mut cancelled = self.lock_cancelled();
                Self::prune_cancelled(&mut cancelled);
                if cancelled
                    .remove(&(conversation_id.clone(), turn_id))
                    .is_some()
                {
                    return Err(info);
                }
            }
            if let Some(existing) = turns.get(&conversation_id).cloned() {
                let finished = existing.finished.load(Ordering::SeqCst);
                // An in-flight generation still owns the slot. A cancelled
                // (or finished) one must not; Stop would otherwise 409 every
                // later prompt onto the dead turn.
                if !finished && !existing.cancel_requested() {
                    return Err(existing.snapshot_info());
                }
                if !finished {
                    existing.request_cancel();
                    displaced = existing.take_pump();
                    displaced_turn = Some(existing);
                }
            }
            let turn = Arc::new(LiveTurn::new(info));
            turns.insert(conversation_id.clone(), Arc::clone(&turn));
            turn
        };
        if let Some(old) = displaced_turn
            && !old.finished.load(Ordering::SeqCst)
        {
            old.mark_cancelled_done();
        }
        if let Some(handle) = displaced {
            handle.abort();
        }
        let pump = spawn_pump(self.clone(), Arc::clone(&turn), source, lease);
        turn.set_pump(pump);
        // Cancel can drop us from the hub after insert and before set_pump.
        // Abort the worker so it cannot keep an upstream LLM slot occupied.
        if turn.cancel_requested() || !self.holds(&turn) {
            if let Some(handle) = turn.take_pump() {
                handle.abort();
            }
            if !turn.finished.load(Ordering::SeqCst) {
                turn.mark_cancelled_done();
            }
            self.drop_if_same(&turn);
        }
        Ok(turn.subscribe_stream())
    }

    fn holds(&self, turn: &Arc<LiveTurn>) -> bool {
        let id = &turn.info.conversation_id;
        self.lock_turns()
            .get(id)
            .is_some_and(|held| Arc::ptr_eq(held, turn))
    }

    fn drop_if_same(&self, turn: &Arc<LiveTurn>) {
        let id = turn.snapshot_info().conversation_id;
        let mut turns = self.lock_turns();
        if turns.get(&id).is_some_and(|held| Arc::ptr_eq(held, turn)) {
            turns.remove(&id);
        }
    }
}

fn spawn_pump(
    hub: LiveHub,
    turn: Arc<LiveTurn>,
    mut source: ChatStream,
    mut lease: crate::session::Lease,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut cancel_rx = turn.cancel.subscribe();
        let mut cancelled = false;
        loop {
            if *cancel_rx.borrow() {
                cancelled = true;
                break;
            }
            tokio::select! {
                biased;
                _ = lease.revoked() => {
                    cancelled = true;
                    break;
                }
                _ = cancel_rx.changed() => {
                    if *cancel_rx.borrow() {
                        cancelled = true;
                        break;
                    }
                }
                item = source.next() => {
                    match item {
                        Some(Ok(bytes)) => {
                            if !turn.push_bytes(bytes) {
                                turn.push_terminal_bytes(sse_error(
                                    "Live response exceeded the 32 MiB replay limit",
                                ));
                                break;
                            }
                        }
                        Some(Err(error)) => {
                            turn.push_terminal_bytes(sse_error(&error.to_string()));
                            break;
                        }
                        None => break,
                    }
                }
            }
        }
        drop(source);
        if cancelled {
            turn.push_terminal_bytes(b"data: [DONE]\n\n".to_vec());
        }
        turn.finish();
        if cancelled {
            hub.drop_if_same(&turn);
            return;
        }
        tokio::time::sleep(LINGER).await;
        hub.drop_if_same(&turn);
    })
}

static HUB: OnceLock<LiveHub> = OnceLock::new();

pub fn hub() -> &'static LiveHub {
    HUB.get_or_init(LiveHub::new)
}

pub fn new_turn_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_ok() {
        return format!("turn_{}", hex_bytes(&bytes));
    }

    // A random-source failure must not collapse every turn onto the same all-zero id.
    static FALLBACK_COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let count = FALLBACK_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("turn_{nanos:032x}{count:016x}")
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{StreamExt, stream};
    use std::io;

    fn info(id: &str) -> LiveTurnInfo {
        LiveTurnInfo {
            conversation_id: id.into(),
            turn_id: "turn_test".into(),
            agent: false,
            deep_research: false,
            deep_research_output: "long".into(),
            model: "test-model".into(),
            finished: false,
        }
    }

    fn source(frames: Vec<&'static [u8]>) -> ChatStream {
        Box::pin(stream::iter(
            frames
                .into_iter()
                .map(|bytes| Ok::<Vec<u8>, io::Error>(bytes.to_vec())),
        ))
    }

    async fn collect(stream: ChatStream) -> Vec<Vec<u8>> {
        stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|item| item.expect("frame"))
            .collect()
    }

    #[test]
    fn replay_after_clones_only_the_new_suffix() {
        let turn = LiveTurn::new(info("replay"));
        for index in 0..5 {
            assert!(turn.push_bytes(format!("frame-{index}").into_bytes()));
        }

        let (frames, next) = turn.replay_after(3);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].seq, 4);
        assert_eq!(frames[1].seq, 5);
        assert_eq!(next, 5);

        let (none, unchanged) = turn.replay_after(5);
        assert!(none.is_empty());
        assert_eq!(unchanged, 5);
    }

    #[test]
    fn live_info_finished_state_comes_from_the_atomic() {
        let turn = LiveTurn::new(info("meta"));
        assert!(!turn.snapshot_info().finished);
        turn.finish();
        assert!(turn.snapshot_info().finished);
    }

    #[tokio::test]
    async fn replay_then_end_for_a_finished_turn() {
        let hub = LiveHub::new();
        let stream = hub
            .start(
                info("c1"),
                source(vec![
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n",
                    b"data: [DONE]\n\n",
                ]),
            )
            .expect("start");
        let frames = collect(stream).await;
        let joined = frames
            .iter()
            .map(|f| String::from_utf8_lossy(f).into_owned())
            .collect::<String>();
        assert!(joined.contains("event: meta"));
        assert!(joined.contains("event: live"));
        assert!(joined.contains("Hi"));
        assert!(joined.contains("[DONE]"));
    }

    #[tokio::test]
    async fn second_start_joins_existing() {
        let hub = LiveHub::new();
        let first = hub
            .start(info("c2"), source(vec![b"data: [DONE]\n\n"]))
            .expect("start");
        let err = hub
            .start(info("c2"), source(vec![b"data: nope\n\n"]))
            .err()
            .expect("duplicate");
        assert_eq!(err.conversation_id, "c2");
        drop(first);
    }

    #[tokio::test]
    async fn cancel_stops_a_hanging_source() {
        let hub = LiveHub::new();
        let hanging: ChatStream = Box::pin(async_stream::stream! {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                yield Ok::<Vec<u8>, io::Error>(b"data: tick\n\n".to_vec());
            }
        });
        let sub = hub.start(info("c3"), hanging).expect("start");
        assert!(hub.cancel("c3", Some("turn_test")));
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        loop {
            let gone = hub.info("c3").is_none();
            let finished = hub
                .list()
                .iter()
                .any(|item| item.conversation_id == "c3" && item.finished);
            if gone || finished {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("cancelled turn did not finish");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let joined = collect(sub)
            .await
            .into_iter()
            .map(|frame| String::from_utf8_lossy(&frame).into_owned())
            .collect::<String>();
        assert!(joined.contains("[DONE]"));
    }

    #[tokio::test]
    async fn cancelled_turn_is_dropped_without_waiting_for_linger() {
        let hub = LiveHub::new();
        let _first = hub
            .start(info("c8"), Box::pin(futures_util::stream::pending()))
            .expect("start");
        let (cancelled, settled) = hub.cancel_and_wait("c8", Some("turn_test")).await;
        assert!(cancelled);
        assert!(settled);
        assert!(hub.info("c8").is_none());
    }

    #[tokio::test]
    async fn start_replaces_a_cancelled_unfinished_turn() {
        let hub = LiveHub::new();
        let first = hub
            .start(info("c9"), Box::pin(futures_util::stream::pending()))
            .expect("first start");
        assert!(hub.cancel("c9", Some("turn_test")));

        let mut next_info = info("c9");
        next_info.turn_id = "turn_new".into();
        let replacement = hub
            .start(next_info, source(vec![b"data: next\n\n"]))
            .expect("a cancelled turn must not 409 the next prompt");
        let replacement_text = collect(replacement)
            .await
            .into_iter()
            .map(|frame| String::from_utf8_lossy(&frame).into_owned())
            .collect::<String>();
        assert!(replacement_text.contains("next"));

        let first_text = collect(first)
            .await
            .into_iter()
            .map(|frame| String::from_utf8_lossy(&frame).into_owned())
            .collect::<String>();
        assert!(first_text.contains("[DONE]"));
    }

    #[tokio::test]
    async fn replacement_starts_only_after_cancelled_turn_settles() {
        let hub = LiveHub::new();
        let first = hub
            .start(info("c4"), Box::pin(futures_util::stream::pending()))
            .expect("first start");

        let (cancelled, settled) = hub.cancel_and_wait("c4", Some("turn_test")).await;
        assert!(cancelled);
        assert!(settled);

        let mut replacement_info = info("c4");
        replacement_info.turn_id = "turn_replacement".into();
        let replacement = hub
            .start(replacement_info, source(vec![b"data: replacement\n\n"]))
            .expect("settled cancellation must allow replacement");
        let replacement_text = collect(replacement)
            .await
            .into_iter()
            .map(|frame| String::from_utf8_lossy(&frame).into_owned())
            .collect::<String>();
        assert!(replacement_text.contains("replacement"));

        let first_text = collect(first)
            .await
            .into_iter()
            .map(|frame| String::from_utf8_lossy(&frame).into_owned())
            .collect::<String>();
        assert!(first_text.contains("[DONE]"));
    }

    #[tokio::test]
    async fn cancel_before_registration_cannot_create_an_orphan_turn() {
        let hub = LiveHub::new();
        let (cancelled, settled) = hub.cancel_and_wait("race", Some("turn_old")).await;
        assert!(!cancelled);
        assert!(settled);

        let mut late = info("race");
        late.turn_id = "turn_old".into();
        assert!(
            hub.start(late, Box::pin(futures_util::stream::pending()))
                .is_err()
        );

        let mut replacement = info("race");
        replacement.turn_id = "turn_new".into();
        let stream = hub
            .start(replacement, source(vec![b"data: replacement\n\n"]))
            .expect("new turn must not be blocked by old tombstone");
        let text = collect(stream)
            .await
            .into_iter()
            .map(|frame| String::from_utf8_lossy(&frame).into_owned())
            .collect::<String>();
        assert!(text.contains("replacement"));
    }

    #[tokio::test]
    async fn finished_turn_does_not_block_the_next_turn() {
        let hub = LiveHub::new();
        let first = hub
            .start(info("c4"), source(vec![b"data: first\n\n"]))
            .expect("first start");
        let _ = collect(first).await;
        assert!(!hub.cancel("c4", Some("turn_test")));

        let mut second_info = info("c4");
        second_info.turn_id = "turn_second".into();
        let second = hub
            .start(second_info, source(vec![b"data: second\n\n"]))
            .expect("finished turn should be replaceable");
        let joined = collect(second)
            .await
            .into_iter()
            .map(|frame| String::from_utf8_lossy(&frame).into_owned())
            .collect::<String>();
        assert!(joined.contains("second"));
        assert!(!joined.contains("data: first"));
    }

    #[tokio::test]
    async fn stale_turn_id_cannot_cancel_a_replacement() {
        let hub = LiveHub::new();
        let first = hub
            .start(info("c5"), source(vec![b"data: first\n\n"]))
            .expect("first start");
        let _ = collect(first).await;

        let mut second_info = info("c5");
        second_info.turn_id = "turn_second".into();
        let _second = hub
            .start(second_info, Box::pin(futures_util::stream::pending()))
            .expect("second start");
        assert!(!hub.cancel("c5", Some("turn_test")));
        assert!(hub.cancel("c5", Some("turn_second")));
    }

    #[tokio::test]
    async fn finished_turn_is_removed_from_the_hub_that_started_it() {
        let hub = LiveHub::new();
        let subscriber = hub
            .start(info("c6"), source(vec![b"data: done\n\n"]))
            .expect("start");
        let _ = collect(subscriber).await;
        tokio::time::sleep(LINGER + Duration::from_millis(20)).await;
        assert!(hub.info("c6").is_none());
    }

    #[test]
    fn replay_buffer_enforces_byte_and_frame_limits() {
        assert!(within_replay_limit(0, 0, 1));
        assert!(within_replay_limit(
            MAX_BUFFERED_BYTES - 1,
            MAX_BUFFERED_FRAMES - 1,
            1
        ));
        assert!(!within_replay_limit(MAX_BUFFERED_BYTES, 0, 1));
        assert!(!within_replay_limit(0, MAX_BUFFERED_FRAMES, 1));
    }
}
