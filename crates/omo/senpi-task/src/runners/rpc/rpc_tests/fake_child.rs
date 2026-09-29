//! Rust port of `__fixtures__/fake-child.mjs` and `process-tree.mjs`: the test binary re-executes
//! itself into [`fake_child_entry`], which speaks the senpi RPC wire protocol on stdio.
//!
//! Adaptation: the malformed-line and UI-request emissions fire on the first command instead of at
//! startup, so a Rust subscriber cannot miss them (Node delivered them after the subscribing tick).

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::runners::rpc::process::RpcChildProcess;
use crate::runners::rpc::terminate::terminate_rpc_child;
use crate::runners::types::TerminateOptions;

const MODE_ENV: &str = "SENPI_TASK_FAKE_CHILD";
const ENTRY_TEST: &str = "runners::rpc::rpc_tests::fake_child::fake_child_entry";
const SAFETY_EXIT: Duration = Duration::from_secs(30);

static EMIT: Mutex<()> = Mutex::new(());

fn emit(payload: &Value) {
    let _guard = EMIT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{payload}");
    let _ = stdout.flush();
}

fn respond(command: &str, id: &Value, extra: Option<Value>) {
    let mut response = json!({ "type": "response", "command": command, "id": id, "success": true });
    if let (Some(target), Some(Value::Object(extra))) = (response.as_object_mut(), extra) {
        target.extend(extra);
    }
    emit(&response);
}

fn assistant_message(text: &str, stop_reason: &str, error_message: Option<&str>) -> Value {
    let content = if text.is_empty() {
        json!([])
    } else {
        json!([{ "type": "text", "text": text }])
    };
    let mut message = json!({ "role": "assistant", "content": content, "stopReason": stop_reason });
    if let (Some(target), Some(error)) = (message.as_object_mut(), error_message) {
        target.insert("errorMessage".to_string(), json!(error));
    }
    message
}

fn complete_turn(text: &str) {
    let message = assistant_message(text, "endTurn", None);
    emit(&json!({ "type": "message_end", "message": message }));
    emit(&json!({ "type": "agent_end", "willRetry": false, "messages": [message] }));
}

fn exit_now(code: i32) -> ! {
    let _ = io::stdout().flush();
    let _ = io::stderr().flush();
    std::process::exit(code)
}

fn ignore_sigterm() {
    // SAFETY: installing SIG_IGN for SIGTERM has no preconditions.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
}

fn handle_prompt(command: &Value, id: &Value) {
    let message = command.get("message").and_then(Value::as_str).unwrap_or("");
    if command.get("streamingBehavior").and_then(Value::as_str) == Some("followUp") {
        respond("prompt", id, None);
        emit(&json!({ "type": "queue_update", "steering": [], "followUp": [message] }));
        if message == "empty-followup" {
            emit(&json!({ "type": "agent_start" }));
            emit(&json!({ "type": "agent_end", "willRetry": false, "messages": [] }));
        }
        return;
    }
    if let Some(rest) = message.strip_prefix("delay:") {
        let ms = rest
            .split(':')
            .next()
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(0);
        let id = id.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(ms));
            respond("prompt", &id, None);
        });
        return;
    }
    if let Some(rest) = message.strip_prefix("crash:") {
        let (code, detail) = rest.split_once(':').unwrap_or((rest, ""));
        let _ = io::stderr().write_all(detail.as_bytes());
        exit_now(code.parse().unwrap_or(1));
    }
    if let Some(error) = message.strip_prefix("prompt-error:") {
        emit(
            &json!({ "type": "response", "command": "prompt", "id": id, "success": false, "error": error }),
        );
        return;
    }
    if let Some(error) = message.strip_prefix("turn-error:") {
        let failed = assistant_message("", "error", Some(error));
        respond("prompt", id, None);
        emit(&json!({ "type": "agent_start" }));
        emit(&json!({ "type": "message_end", "message": failed }));
        emit(&json!({ "type": "agent_end", "willRetry": false, "messages": [failed] }));
        return;
    }
    if let Some(code) = message.strip_prefix("exit:") {
        respond("prompt", id, None);
        exit_now(code.parse().unwrap_or(1));
    }
    if message == "diesignal" {
        respond("prompt", id, None);
        emit(&json!({ "type": "agent_start" }));
        // SAFETY: signalling our own pid.
        unsafe {
            libc::kill(libc::getpid(), libc::SIGKILL);
        }
        return;
    }
    if let Some(text) = message.strip_prefix("finish:") {
        respond("prompt", id, None);
        emit(&json!({ "type": "agent_start" }));
        complete_turn(text);
        thread::sleep(Duration::from_millis(20));
        exit_now(0);
    }
    respond("prompt", id, None);
    emit(&json!({ "type": "agent_start" }));
    if message != "hold" {
        complete_turn(message);
    }
}

fn handle_command(command: &Value) {
    let id = command.get("id").cloned().unwrap_or(Value::Null);
    let kind = command
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = command.get("message").cloned().unwrap_or(Value::Null);
    match kind {
        "prompt" => handle_prompt(command, &id),
        "steer" => {
            respond("steer", &id, None);
            emit(&json!({ "type": "queue_update", "steering": [message], "followUp": [] }));
            if message == "complete" {
                complete_turn("steered-complete");
            }
        }
        "follow_up" => {
            respond("follow_up", &id, None);
            emit(&json!({ "type": "queue_update", "steering": [], "followUp": [message] }));
        }
        "abort" => {
            respond("abort", &id, None);
            emit(&json!({ "type": "agent_end", "willRetry": false, "messages": [] }));
        }
        "get_state" => respond(
            "get_state",
            &id,
            Some(json!({ "data": {
                "sessionId": "fake-session", "thinkingLevel": "medium", "isStreaming": false,
                "isCompacting": false, "steeringMode": "all", "followUpMode": "all",
                "autoCompactionEnabled": false, "messageCount": 1, "pendingMessageCount": 0
            } })),
        ),
        other => respond(other, &id, None),
    }
}

fn handle_session_command(command: &Value) {
    let id = command.get("id").cloned().unwrap_or(Value::Null);
    let kind = command
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    emit(&json!({ "type": "session_info_changed", "name": format!("command:{kind}") }));
    match kind {
        "switch_session" => {
            let path = command
                .get("sessionPath")
                .and_then(Value::as_str)
                .unwrap_or("");
            respond(
                kind,
                &id,
                Some(json!({ "data": { "cancelled": path.contains("cancel") } })),
            );
        }
        "get_entries" => {
            let since = command.get("since").cloned().unwrap_or(Value::Null);
            respond(
                kind,
                &id,
                Some(json!({ "data": { "entries": [], "leafId": since } })),
            );
        }
        _ => respond(kind, &id, None),
    }
}

fn run_rpc(session_mode: bool) {
    let ignore_term = std::env::var("FAKE_IGNORE_TERM").as_deref() == Ok("1");
    let emit_malformed = std::env::var("FAKE_EMIT_MALFORMED").as_deref() == Ok("1");
    let emit_ui = std::env::var("FAKE_EMIT_UI").as_deref() == Ok("1");
    emit(&json!({ "type": "fake_boot" }));
    if ignore_term {
        ignore_sigterm();
        emit(&json!({ "type": "session_info_changed", "name": "ready" }));
    }
    let mut first_command = true;
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if parsed.get("type").and_then(Value::as_str) == Some("extension_ui_response") {
            let outcome = if parsed.get("confirmed") == Some(&Value::Bool(false)) {
                "denied"
            } else if parsed.get("cancelled") == Some(&Value::Bool(true)) {
                "cancelled"
            } else {
                "confirmed"
            };
            emit(&json!({ "type": "session_info_changed", "name": format!("ui:{outcome}") }));
            continue;
        }
        if first_command {
            first_command = false;
            if emit_malformed {
                let _guard = EMIT
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let _ = writeln!(io::stdout().lock(), "this-is-not-json");
            }
            if emit_malformed {
                emit(&json!({ "type": "agent_start" }));
            }
            if emit_ui {
                emit(
                    &json!({ "type": "extension_ui_request", "id": "ui-1", "method": "confirm", "title": "t", "message": "m" }),
                );
            }
        }
        if session_mode {
            handle_session_command(&parsed);
        } else {
            handle_command(&parsed);
        }
    }
    if ignore_term {
        thread::sleep(SAFETY_EXIT);
    }
    exit_now(0);
}

fn run_process_tree(descendant: bool) {
    ignore_sigterm();
    if descendant {
        let _ = writeln!(io::stdout().lock(), "READY");
    } else {
        let spawned = Command::new(std::env::current_exe().unwrap_or_default())
            .args([
                ENTRY_TEST,
                "--exact",
                "--nocapture",
                "--test-threads=1",
                "-q",
            ])
            .env(MODE_ENV, "descendant")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(child) => {
                let _ = writeln!(io::stdout().lock(), "PID:{}", child.id());
            }
            Err(error) => {
                let _ = writeln!(io::stderr(), "descendant spawn failed: {error}");
                exit_now(1);
            }
        }
    }
    let _ = io::stdout().flush();
    thread::sleep(SAFETY_EXIT);
    exit_now(0);
}

fn run_stderr_flood() {
    let chunk = "x".repeat(1_000);
    for _ in 0..50 {
        let _ = io::stderr().write_all(chunk.as_bytes());
    }
    let _ = io::stderr().flush();
    for line in io::stdin().lock().lines() {
        if line.is_err() {
            break;
        }
    }
    exit_now(0);
}

#[test]
fn fake_child_entry() {
    match std::env::var(MODE_ENV).as_deref() {
        Ok("rpc") => run_rpc(false),
        Ok("session") => run_rpc(true),
        Ok("tree") => run_process_tree(false),
        Ok("descendant") => run_process_tree(true),
        Ok("stderr-flood") => run_stderr_flood(),
        _ => {}
    }
}

/// A command that re-executes this test binary into `mode`.
pub(super) fn fake_command(mode: &str, env: &BTreeMap<String, String>, own_group: bool) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap_or_default());
    command
        .args([
            ENTRY_TEST,
            "--exact",
            "--nocapture",
            "--test-threads=1",
            "-q",
        ])
        .envs(env)
        .env(MODE_ENV, mode);
    #[cfg(unix)]
    if own_group {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

/// `spawnFakeChild`: the RPC fake, detached into its own process group.
pub(super) fn spawn_fake_child(env: &[(&str, &str)]) -> Arc<RpcChildProcess> {
    let env = env
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    Arc::new(RpcChildProcess::spawn_command(fake_command(
        "rpc", &env, true,
    )))
}

/// Terminates the wrapped child when the test ends, pass or fail (`afterEach` in TypeScript).
pub(super) struct ChildGuard(pub Arc<RpcChildProcess>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = terminate_rpc_child(
            &self.0,
            TerminateOptions {
                sigkill_delay_ms: Some(200),
            },
        );
    }
}

/// Wait until `predicate` holds, failing after `timeout` (`waitFor` in the TypeScript tests).
pub(super) fn wait_for(timeout: Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !predicate() {
        assert!(Instant::now() < deadline, "timed out waiting for condition");
        thread::sleep(Duration::from_millis(5));
    }
}

/// `ps -o stat= -p <pid>`: running and not a zombie.
pub(super) fn is_running(pid: u32) -> bool {
    Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map(|output| {
            output.status.success()
                && !String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .starts_with('Z')
        })
        .unwrap_or(false)
}
