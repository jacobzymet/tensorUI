//! Resumable, conversation-owned agent processes, separate from the user's PTY.

use super::{CommandResult, hide_window, kill_process_tree, new_id, shell_command};
use crate::agent::{
    fs::Workspace,
    output::{BoundedOutput, TOOL_OUTPUT_BYTES},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::watch,
};

const MAX_RUNNING_PER_OWNER: usize = 8;
const MAX_SESSIONS: usize = 64;
const IDLE_LIFETIME: Duration = Duration::from_secs(30 * 60);

static COMMANDS: OnceLock<tokio::sync::Mutex<HashMap<String, Arc<Slot>>>> = OnceLock::new();
fn commands() -> &'static tokio::sync::Mutex<HashMap<String, Arc<Slot>>> {
    COMMANDS.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

struct Slot {
    owner: String,
    workspace: PathBuf,
    command: String,
    cwd: String,
    state: Mutex<State>,
    changed: watch::Sender<u64>,
    cancel: watch::Sender<bool>,
}

struct State {
    output: BoundedOutput,
    running: bool,
    exit_code: Option<i32>,
    ok: bool,
    accessed: Instant,
    delivered: bool,
}

pub fn yield_ms(args: &Value, default_ms: u64) -> u64 {
    args.get("yield_time_ms")
        .and_then(Value::as_u64)
        .unwrap_or(default_ms)
        .min(30_000)
}

pub async fn start(
    command: &str,
    cwd: &Path,
    workspace: &Workspace,
    owner: &str,
    wait_ms: u64,
) -> Result<CommandResult, String> {
    if owner.is_empty() {
        return Err("A conversation session is required to start an agent command.".into());
    }
    let workspace_path = workspace.resolve(".")?;
    let mut slots = commands().lock().await;
    slots.retain(|_, slot| {
        let expired = slot
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .accessed
            .elapsed()
            > IDLE_LIFETIME;
        if expired {
            let _ = slot.cancel.send(true);
        }
        !expired
    });
    if slots.len() >= MAX_SESSIONS {
        let oldest = slots
            .iter()
            .filter_map(|(id, slot)| {
                let state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
                (!state.running && state.delivered).then_some((id.clone(), state.accessed))
            })
            .min_by_key(|(_, accessed)| *accessed)
            .map(|(id, _)| id);
        if let Some(id) = oldest {
            slots.remove(&id);
        }
    }
    if slots.len() >= MAX_SESSIONS
        || slots
            .values()
            .filter(|slot| {
                slot.owner == owner && slot.state.lock().unwrap_or_else(|e| e.into_inner()).running
            })
            .count()
            >= MAX_RUNNING_PER_OWNER
    {
        return Err("Too many agent commands. Poll or terminate an existing session first.".into());
    }
    let mut cmd = shell_command(command);
    cmd.current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    hide_window(&mut cmd);
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .map_err(|err| format!("Could not start command: {err}"))?;
    let pid = child.id().unwrap_or(0);
    let stdout = child.stdout.take().ok_or("Command stdout is unavailable")?;
    let stderr = child.stderr.take().ok_or("Command stderr is unavailable")?;
    let (changed, _) = watch::channel(0);
    let (cancel, mut cancelled) = watch::channel(false);
    let slot = Arc::new(Slot {
        owner: owner.into(),
        workspace: workspace_path,
        command: command.into(),
        cwd: cwd.display().to_string(),
        changed,
        cancel,
        state: Mutex::new(State {
            output: BoundedOutput::new(TOOL_OUTPUT_BYTES),
            running: true,
            exit_code: None,
            ok: false,
            accessed: Instant::now(),
            delivered: false,
        }),
    });
    let id = new_id("cmd");
    slots.insert(id.clone(), slot.clone());
    drop(slots);
    let process = slot.clone();
    tokio::spawn(async move {
        let mut stdout_task = tokio::spawn(drain(stdout, process.clone()));
        let mut stderr_task = tokio::spawn(drain(stderr, process.clone()));
        let mut idle = tokio::time::interval(Duration::from_secs(30));
        let mut stopped = None;
        let status = loop {
            tokio::select! {
                status = child.wait() => break status,
                _ = cancelled.changed() => {
                    stopped = Some("Command terminated by request.");
                    let _ = tokio::task::spawn_blocking(move || kill_process_tree(pid)).await;
                    let _ = child.kill().await;
                    break child.wait().await;
                }
                _ = idle.tick() => {
                    let expired = process.state.lock().unwrap_or_else(|e| e.into_inner()).accessed.elapsed() > IDLE_LIFETIME;
                    if expired {
                        stopped = Some("Command terminated after 30 minutes without a poll.");
                        let _ = tokio::task::spawn_blocking(move || kill_process_tree(pid)).await;
                        let _ = child.kill().await;
                        break child.wait().await;
                    }
                }
            }
        };
        // Drain remaining pipe bytes. A detached child must not hold this
        // session open forever just because it inherited an output handle.
        let drained = tokio::time::timeout(Duration::from_secs(1), async {
            let _ = (&mut stdout_task).await;
            let _ = (&mut stderr_task).await;
        })
        .await
        .is_ok();
        if !drained {
            stdout_task.abort();
            stderr_task.abort();
        }
        {
            let mut state = process.state.lock().unwrap_or_else(|e| e.into_inner());
            state.running = false;
            match status {
                Ok(status) => {
                    state.exit_code = status.code();
                    state.ok = status.success() && stopped.is_none();
                }
                Err(err) => {
                    state
                        .output
                        .push(format!("\nCommand failed: {err}\n").as_bytes());
                }
            }
            if let Some(reason) = stopped {
                state.output.push(format!("\n{reason}\n").as_bytes());
            }
            if !drained {
                state
                    .output
                    .push(b"\nOutput pipes remained open after process exit; capture stopped.\n");
            }
        }
        process
            .changed
            .send_modify(|version| *version = version.wrapping_add(1));
    });
    poll_slot(&id, slot, wait_ms).await
}

async fn drain(mut reader: impl AsyncRead + Unpin, slot: Arc<Slot>) {
    let mut buf = [0u8; 8192];
    let mut pending = Vec::new();
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => {
                if !pending.is_empty() {
                    slot.state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .output
                        .push(String::from_utf8_lossy(&pending).as_bytes());
                }
                break;
            }
            Ok(n) => {
                pending.extend_from_slice(&buf[..n]);
                let text = decode_complete_utf8(&mut pending);
                slot.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .output
                    .push(text.as_bytes());
                slot.changed
                    .send_modify(|version| *version = version.wrapping_add(1));
            }
            Err(err) => {
                slot.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .output
                    .push(format!("\nOutput read failed: {err}\n").as_bytes());
                break;
            }
        }
    }
}

// Preserve characters across pipe reads and polling, even when stdout and
// stderr chunks interleave. At most three incomplete UTF-8 bytes remain.
fn decode_complete_utf8(pending: &mut Vec<u8>) -> String {
    let mut consumed = 0;
    let mut text = String::new();
    while consumed < pending.len() {
        match std::str::from_utf8(&pending[consumed..]) {
            Ok(valid) => {
                text.push_str(valid);
                consumed = pending.len();
            }
            Err(err) => {
                let end = consumed + err.valid_up_to();
                text.push_str(
                    std::str::from_utf8(&pending[consumed..end]).expect("validated UTF-8 prefix"),
                );
                consumed = end;
                if let Some(invalid) = err.error_len() {
                    text.push('\u{fffd}');
                    consumed += invalid;
                } else {
                    break;
                }
            }
        }
    }
    pending.drain(..consumed);
    text
}

pub async fn poll(
    workspace_root: &str,
    owner: &str,
    args: &Value,
) -> Result<CommandResult, String> {
    let id = args
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or("wait_terminal requires session_id")?;
    let slot = commands()
        .lock()
        .await
        .get(id)
        .cloned()
        .ok_or("Command session not found or expired; do not assume it is still running.")?;
    let workspace = Workspace::open(workspace_root)?;
    if owner.is_empty() || owner != slot.owner || workspace.resolve(".")? != slot.workspace {
        return Err("Command session belongs to another conversation or workspace.".into());
    }
    if args
        .get("terminate")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let _ = slot.cancel.send(true);
    }
    poll_slot(id, slot, yield_ms(args, 1000)).await
}

async fn poll_slot(id: &str, slot: Arc<Slot>, wait_ms: u64) -> Result<CommandResult, String> {
    let mut changed = slot.changed.subscribe();
    let should_wait = {
        let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
        state.accessed = Instant::now();
        state.running && state.output.is_empty()
    };
    if should_wait && wait_ms > 0 {
        let _ = tokio::time::timeout(
            Duration::from_millis(wait_ms.min(30_000)),
            changed.changed(),
        )
        .await;
    }
    let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
    let output = if state.running {
        state.output.take()
    } else {
        state.output.render()
    };
    if !state.running {
        state.delivered = true;
    }
    Ok(CommandResult {
        command: slot.command.clone(),
        cwd: slot.cwd.clone(),
        output,
        exit_code: state.exit_code,
        ok: state.running || state.ok,
        timed_out: false,
        session_id: Some(id.into()),
        running: state.running,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn output_characters_survive_arbitrary_pipe_boundaries() {
        let mut pending = Vec::new();
        let mut output = String::new();
        for byte in "hello é🙂你好\r\n".bytes() {
            pending.push(byte);
            output.push_str(&decode_complete_utf8(&mut pending));
            assert!(pending.len() <= 3);
        }
        assert!(pending.is_empty());
        assert_eq!(output, "hello é🙂你好\r\n");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn powershell_sessions_preserve_quotes_unicode_and_native_failures() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        let owner = new_id("test");
        for (command, code, expected) in [
            (
                "$value = 'it''s $literal 你好'; Write-Output $value",
                0,
                "it's $literal 你好",
            ),
            ("cmd.exe /d /c exit 9", 9, ""),
            ("Write-Error 'session failure'", 1, "session failure"),
        ] {
            let mut result = start(command, dir.path(), &ws, &owner, 0).await.unwrap();
            let id = result.session_id.clone().unwrap();
            let mut output = result.output.clone();
            let deadline = Instant::now() + Duration::from_secs(10);
            while result.running {
                assert!(Instant::now() < deadline);
                result = poll(
                    &ws.root_display(),
                    &owner,
                    &json!({"session_id": id, "yield_time_ms":1000}),
                )
                .await
                .unwrap();
                output.push_str(&result.output);
            }
            assert_eq!(result.exit_code, Some(code), "{command}: {output}");
            assert_eq!(result.ok, code == 0);
            assert!(output.contains(expected), "{output}");
        }
    }

    #[tokio::test]
    async fn yields_then_resumes_the_same_process_and_retains_its_final_error() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        let owner = new_id("test");
        let command = if cfg!(windows) {
            "Write-Output 'start'; Start-Sleep -Milliseconds 300; Write-Output 'end'; exit 7"
        } else {
            "echo start; sleep 0.3; echo end; exit 7"
        };
        let first = start(command, dir.path(), &ws, &owner, 0).await.unwrap();
        assert!(first.running);
        let id = first.session_id.unwrap();
        let mut output = first.output;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline);
            let result = poll(
                &ws.root_display(),
                &owner,
                &json!({"session_id": id, "yield_time_ms": 1000}),
            )
            .await
            .unwrap();
            output.push_str(&result.output);
            if !result.running {
                assert_eq!(result.exit_code, Some(7));
                assert!(!result.ok);
                break;
            }
        }
        assert!(output.contains("start"));
        assert!(output.contains("end"));
    }

    #[tokio::test]
    async fn sessions_are_scoped_and_can_be_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        let owner = new_id("test");
        let command = if cfg!(windows) {
            "Start-Sleep -Seconds 60"
        } else {
            "sleep 60"
        };
        let first = start(command, dir.path(), &ws, &owner, 0).await.unwrap();
        let id = first.session_id.unwrap();
        assert!(
            poll(
                &ws.root_display(),
                "wrong-owner",
                &json!({"session_id": id, "terminate": true})
            )
            .await
            .is_err()
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline);
            let result = poll(
                &ws.root_display(),
                &owner,
                &json!({"session_id": id, "terminate": true, "yield_time_ms": 1000}),
            )
            .await
            .unwrap();
            if !result.running {
                assert!(!result.ok);
                assert!(result.output.contains("terminated"));
                break;
            }
        }
    }
}
