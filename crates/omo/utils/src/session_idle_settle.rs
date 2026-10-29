//! Settle delay and busy-status probe used before re-prompting an idle session.

use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use serde_json::Value;

pub const DEFAULT_SESSION_IDLE_SETTLE_MS: u64 = 150;
pub const DEFAULT_SESSION_STATUS_TIMEOUT_MS: u64 = 5_000;

/// Source of `session.status()` payloads (`{ [sessionID]: { type } }`, optionally under `data`).
pub trait SessionStatusClient: Send + Sync {
    fn session_status(&self) -> Option<Result<Value, String>>;
}

pub fn settle_after_session_idle(ms: u64) {
    if ms > 0 {
        thread::sleep(Duration::from_millis(ms));
    }
}

pub fn is_active_session_status_type(status_type: &str) -> bool {
    matches!(status_type, "busy" | "retry" | "running")
}

fn payload(response: &Value) -> Option<&serde_json::Map<String, Value>> {
    response
        .get("data")
        .and_then(Value::as_object)
        .or_else(|| response.as_object())
}

pub fn is_session_active(
    client: Arc<dyn SessionStatusClient>,
    session_id: &str,
    status_timeout_ms: u64,
) -> bool {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(client.session_status());
    });
    let Ok(Some(Ok(response))) = rx.recv_timeout(Duration::from_millis(status_timeout_ms)) else {
        return false;
    };
    payload(&response)
        .and_then(|statuses| statuses.get(session_id))
        .and_then(|status| status.get("type"))
        .and_then(Value::as_str)
        .is_some_and(is_active_session_status_type)
}

pub fn should_prompt_after_session_idle(
    client: Arc<dyn SessionStatusClient>,
    session_id: &str,
    settle_ms: u64,
) -> bool {
    settle_after_session_idle(settle_ms);
    !is_session_active(client, session_id, DEFAULT_SESSION_STATUS_TIMEOUT_MS)
}
