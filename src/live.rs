//! Detached chat turns: generation keeps running after a browser refresh,
//! and any tab can subscribe to the same conversation's live SSE.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::StreamExt;
use serde::Serialize;
use tokio::sync::{broadcast, watch};

use crate::agent::chat::{ChatStream, sse_error};

const BROADCAST_CAP: usize = 256;
const MAX_BUFFERED_BYTES: u64 = 32 * 1024 * 1024;
const MAX_BUFFERED_FRAMES: usize = 100_000;
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
    info: Mutex<LiveTurnInfo>,
    frames: Mutex<Vec<StoredFrame>>,
    buffered_bytes: AtomicU64,
    seq: AtomicU64,
    tx: broadcast::Sender<StoredFrame>,
    cancel: watch::Sender<bool>,
    done: watch::Sender<bool>,
    finished: AtomicBool,
}

impl LiveTurn {
    fn new(mut info: LiveTurnInfo) -> Self {
        info.finished = false;
        let (tx, _) = broadcast::channel(BROADCAST_CAP);
        let (cancel, _) = watch::channel(false);
        let (done, _) = watch::channel(false);
        Self {
            info: Mutex::new(info),
            frames: Mutex::new(Vec::new()),
            buffered_bytes: AtomicU64::new(0),
            seq: AtomicU64::new(0),
            tx,
            cancel,
            done,
            finished: AtomicBool::new(false),
        }
    }

    fn snapshot_info(&self) -> LiveTurnInfo {
        self.info
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|err| err.into_inner().clone())
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
        if let Ok(mut info) = self.info.lock() {
            info.finished = true;
        }
        let _ = self.done.send(true);
    }

    fn request_cancel(&self) {
        self.cancel.send_replace(true);
    }

    fn meta_frame(&self) -> Vec<u8> {
        let payload = serde_json::to_string(&self.snapshot_info()).unwrap_or_else(|_| "{}".into());
        format!("event: meta\ndata: {payload}\n\n").into_bytes()
    }

    fn replay_after(self: &Arc<Self>, last_seq: u64) -> (Vec<StoredFrame>, u64) {
        let replay = self
            .frames
            .lock()
            .map(|frames| frames.clone())
            .unwrap_or_default();
        let mut next = last_seq;
        let extra = replay
            .into_iter()
            .filter(|frame| frame.seq > last_seq)
            .inspect(|frame| next = frame.seq)
            .collect();
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
}

impl LiveHub {
    fn new() -> Self {
        Self {
            turns: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn lock_turns(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<LiveTurn>>> {
        self.turns.lock().unwrap_or_else(|err| err.into_inner())
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

    pub fn info(&self, conversation_id: &str) -> Option<LiveTurnInfo> {
        self.lock_turns()
            .get(conversation_id)
            .map(|turn| turn.snapshot_info())
    }

    pub fn cancel(&self, conversation_id: &str, turn_id: Option<&str>) -> bool {
        let Some(turn) = self.lock_turns().get(conversation_id).cloned() else {
            return false;
        };
        if turn_id.is_some_and(|expected| turn.snapshot_info().turn_id != expected) {
            return false;
        }
        if turn.finished.load(Ordering::SeqCst) {
            return false;
        }
        turn.request_cancel();
        true
    }

    pub fn start(
        &self,
        info: LiveTurnInfo,
        source: ChatStream,
    ) -> Result<ChatStream, LiveTurnInfo> {
        let conversation_id = info.conversation_id.clone();
        let turn = {
            let mut turns = self.lock_turns();
            if let Some(existing) = turns.get(&conversation_id)
                && !existing.finished.load(Ordering::SeqCst)
            {
                return Err(existing.snapshot_info());
            }
            let turn = Arc::new(LiveTurn::new(info));
            turns.insert(conversation_id.clone(), Arc::clone(&turn));
            turn
        };
        spawn_pump(self.clone(), Arc::clone(&turn), source);
        Ok(turn.subscribe_stream())
    }

    fn drop_if_same(&self, turn: &Arc<LiveTurn>) {
        let id = turn.snapshot_info().conversation_id;
        let mut turns = self.lock_turns();
        if turns.get(&id).is_some_and(|held| Arc::ptr_eq(held, turn)) {
            turns.remove(&id);
        }
    }
}

fn spawn_pump(hub: LiveHub, turn: Arc<LiveTurn>, mut source: ChatStream) {
    tokio::spawn(async move {
        let mut cancel_rx = turn.cancel.subscribe();
        let mut cancelled = false;
        loop {
            if *cancel_rx.borrow() {
                cancelled = true;
                break;
            }
            tokio::select! {
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
        tokio::time::sleep(LINGER).await;
        hub.drop_if_same(&turn);
    });
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
            if hub
                .list()
                .iter()
                .any(|item| item.conversation_id == "c3" && item.finished)
            {
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
