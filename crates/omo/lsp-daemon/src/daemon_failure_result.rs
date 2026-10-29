//! Port of `daemon-failure-result.ts`: user-facing text for failed daemon calls.

use serde_json::{Value, json};

use crate::daemon_request_error::{DaemonCallError, DaemonRequestErrorKind};
use crate::paths::DaemonPaths;

/// TS `ToolExecutionResult` as returned by the daemon client.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionResult {
    pub content: Vec<Value>,
    pub is_error: bool,
    pub details: Option<Value>,
}

impl ToolExecutionResult {
    fn error_text(text: String) -> Self {
        Self {
            content: vec![json!({"type": "text", "text": text})],
            is_error: true,
            details: None,
        }
    }

    /// First text block (convenience for callers and tests).
    pub fn text(&self) -> &str {
        self.content
            .first()
            .and_then(|block| block.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("")
    }
}

/// TS `daemonFailureResult`.
pub fn daemon_failure_result(paths: &DaemonPaths, error: &DaemonCallError) -> ToolExecutionResult {
    if let DaemonCallError::Request(request) = error {
        match request.kind {
            DaemonRequestErrorKind::Cancelled => return cancelled(paths),
            DaemonRequestErrorKind::TimedOut { timeout_ms } => return timed_out(paths, timeout_ms),
            DaemonRequestErrorKind::Request | DaemonRequestErrorKind::AuthenticationRejected => {}
        }
    }
    unreachable_result(paths, error)
}

fn cancelled(paths: &DaemonPaths) -> ToolExecutionResult {
    ToolExecutionResult::error_text(
        [
            "LSP daemon request cancelled: the caller aborted this request (for example, the turn was interrupted).".to_string(),
            "The daemon stays available; no LSP work was applied. Retry when you are ready.".to_string(),
            format!("Socket: {}", paths.socket.display()),
        ]
        .join("\n"),
    )
}

fn timed_out(paths: &DaemonPaths, timeout_ms: u64) -> ToolExecutionResult {
    ToolExecutionResult::error_text(
        [
            format!("LSP daemon request timed out after {timeout_ms}ms: the daemon did not respond in time."),
            "The daemon stays available but may be busy. Retry when you are ready.".to_string(),
            format!("Socket: {}", paths.socket.display()),
            format!("Logs: {}", paths.log.display()),
        ]
        .join("\n"),
    )
}

fn unreachable_result(paths: &DaemonPaths, error: &DaemonCallError) -> ToolExecutionResult {
    ToolExecutionResult::error_text(
        [
            format!("LSP daemon unreachable: {error}."),
            "The MCP server is a thin proxy and never runs language servers in-process."
                .to_string(),
            format!("Socket: {}", paths.socket.display()),
            format!("Logs: {}", paths.log.display()),
            "The daemon is auto-started on demand and will be retried on the next request."
                .to_string(),
        ]
        .join("\n"),
    )
}

#[cfg(test)]
#[path = "daemon_failure_result_tests.rs"]
mod tests;
