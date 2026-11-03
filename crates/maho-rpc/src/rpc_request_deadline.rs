//! How long a client waits for the host's answer (senpi `rpc/rpc-request-deadline.ts`).
use crate::abort_grace::{ABORT_DEFAULT_GRACE_MS, is_abort_grace_in_range};
use maho_core::compaction::stream_watchdog::{
    SUMMARIZATION_MAX_DURATION_CAP_MS, SUMMARIZATION_TOTAL_BUDGET_MS,
};
use serde_json::Value;

/// How long any request waits for the host's answer - for an open, for its `queued` acknowledgement.
pub const REQUEST_DEADLINE_MS: u64 = 30_000;

/// An awaited abort waits its grace plus the ordinary allowance; a `graceMs` the host would refuse gets
/// the plain deadline (the host answers it at once with `invalid_argument`).
pub fn request_deadline_ms(command: &Value) -> u64 {
    if command.get("type").and_then(Value::as_str) != Some("abort")
        || command.get("awaitIdle") != Some(&Value::Bool(true))
    {
        return REQUEST_DEADLINE_MS;
    }
    match command.get("graceMs") {
        None => ABORT_DEFAULT_GRACE_MS + REQUEST_DEADLINE_MS,
        Some(grace) if is_abort_grace_in_range(grace) => {
            grace.as_f64().map_or(0, |grace| grace as u64) + REQUEST_DEADLINE_MS
        }
        Some(_) => REQUEST_DEADLINE_MS,
    }
}

/// Remote compaction may exhaust its budget before a local summary uses its maximum override.
pub const PROMPT_COMPACTION_DEADLINE_MS: u64 = SUMMARIZATION_TOTAL_BUDGET_MS as u64
    + SUMMARIZATION_MAX_DURATION_CAP_MS as u64
    + REQUEST_DEADLINE_MS;

/// The longest a prompt waits for its acknowledgement, however many compactions start and end.
pub const PROMPT_ACK_MAX_WAIT_MS: u64 = PROMPT_COMPACTION_DEADLINE_MS + REQUEST_DEADLINE_MS;

/// How long an open the host acknowledged with `queued` may take to answer (senpi#2209).
pub const OPEN_AFTER_QUEUED_DEADLINE_MS: u64 = 10 * 60_000;

/// How long a prompt the host acknowledged as received may take to be accepted (senpi#2871).
pub const PROMPT_AFTER_QUEUED_DEADLINE_MS: u64 = 5 * 60_000;

pub fn prompt_stalled_message() -> String {
    format!(
        "prompt was received by the host but not accepted within {} minutes",
        PROMPT_AFTER_QUEUED_DEADLINE_MS / 60_000
    )
}

pub fn open_stalled_message(position: Option<&Value>) -> String {
    let queued = match position {
        Some(Value::Number(position)) => format!(" at queue position {position}"),
        _ => String::new(),
    };
    format!(
        "open_session was accepted by the host{queued} but not answered within {} minutes",
        OPEN_AFTER_QUEUED_DEADLINE_MS / 60_000
    )
}

/// A request the host did not answer within its deadline.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct RpcRequestTimeoutError {
    pub message: String,
}

impl RpcRequestTimeoutError {
    pub const CODE: &'static str = "rpc_request_timeout";
}

/// A restartable deadline: `expire` runs once with the timeout error unless cleared or extended first.
pub struct RequestDeadline {
    timer: Option<tokio::task::JoinHandle<()>>,
    expire: std::sync::Arc<dyn Fn(RpcRequestTimeoutError) + Send + Sync>,
}

impl RequestDeadline {
    fn arm(&mut self, ms: u64, message: Box<dyn FnOnce() -> String + Send>) {
        self.clear();
        let expire = self.expire.clone();
        self.timer = Some(tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            expire(RpcRequestTimeoutError { message: message() });
        }));
    }

    /// Restarts the wait with a new budget and the message it fails with.
    pub fn extend(&mut self, ms: u64, message: impl FnOnce() -> String + Send + 'static) {
        self.arm(ms, Box::new(message));
    }

    pub fn clear(&mut self) {
        if let Some(timer) = self.timer.take() {
            timer.abort();
        }
    }
}

impl Drop for RequestDeadline {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Must be called inside a tokio runtime.
pub fn arm_request_deadline(
    ms: u64,
    message: impl FnOnce() -> String + Send + 'static,
    expire: impl Fn(RpcRequestTimeoutError) + Send + Sync + 'static,
) -> RequestDeadline {
    let mut deadline = RequestDeadline {
        timer: None,
        expire: std::sync::Arc::new(expire),
    };
    deadline.arm(ms, Box::new(message));
    deadline
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn awaited_aborts_wait_their_grace() {
        assert_eq!(
            request_deadline_ms(&json!({"type":"prompt"})),
            REQUEST_DEADLINE_MS
        );
        assert_eq!(
            request_deadline_ms(&json!({"type":"abort"})),
            REQUEST_DEADLINE_MS
        );
        assert_eq!(
            request_deadline_ms(&json!({"type":"abort","awaitIdle":true})),
            40_000
        );
        assert_eq!(
            request_deadline_ms(&json!({"type":"abort","awaitIdle":true,"graceMs":5000})),
            35_000
        );
        assert_eq!(
            request_deadline_ms(&json!({"type":"abort","awaitIdle":true,"graceMs":1.5})),
            REQUEST_DEADLINE_MS
        );
        assert_eq!(
            request_deadline_ms(&json!({"type":"abort","awaitIdle":true,"graceMs":70000})),
            REQUEST_DEADLINE_MS
        );
    }

    #[test]
    fn stalled_messages_and_budgets() {
        assert_eq!(PROMPT_COMPACTION_DEADLINE_MS, 900_000 + 1_800_000 + 30_000);
        assert_eq!(
            open_stalled_message(Some(&json!(3))),
            "open_session was accepted by the host at queue position 3 but not answered within 10 minutes"
        );
        assert_eq!(
            open_stalled_message(None),
            "open_session was accepted by the host but not answered within 10 minutes"
        );
        assert_eq!(
            prompt_stalled_message(),
            "prompt was received by the host but not accepted within 5 minutes"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_deadline_expires_once_and_extend_restarts_it() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut deadline = arm_request_deadline(
            1_000,
            || "first".into(),
            move |error| {
                let _ = sender.send(error.message);
            },
        );
        tokio::time::advance(std::time::Duration::from_millis(500)).await;
        deadline.extend(2_000, || "second".into());
        assert_eq!(receiver.recv().await.as_deref(), Some("second"));
        deadline.clear();
    }
}
