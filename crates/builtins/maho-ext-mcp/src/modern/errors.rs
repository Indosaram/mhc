//! Port of senpi mcp/modern/errors.ts: local MRTR failures with fixed, payload-free messages.
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpMrtrErrorCode {
    RoundLimit,
    InputLimit,
    InvalidInput,
    UnsupportedInput,
    ProcessingUnavailable,
    InvalidResult,
    ContextChanged,
    Cancelled,
    Timeout,
    TransportFailed,
    TaskTimeout,
    TaskFailed,
    TaskCancelled,
}
impl McpMrtrErrorCode {
    pub const ALL: [Self; 13] = [
        Self::RoundLimit,
        Self::InputLimit,
        Self::InvalidInput,
        Self::UnsupportedInput,
        Self::ProcessingUnavailable,
        Self::InvalidResult,
        Self::ContextChanged,
        Self::Cancelled,
        Self::Timeout,
        Self::TransportFailed,
        Self::TaskTimeout,
        Self::TaskFailed,
        Self::TaskCancelled,
    ];
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RoundLimit => "MCP_MRTR_ROUND_LIMIT",
            Self::InputLimit => "MCP_MRTR_INPUT_LIMIT",
            Self::InvalidInput => "MCP_MRTR_INVALID_INPUT",
            Self::UnsupportedInput => "MCP_MRTR_UNSUPPORTED_INPUT",
            Self::ProcessingUnavailable => "MCP_MRTR_PROCESSING_UNAVAILABLE",
            Self::InvalidResult => "MCP_MRTR_INVALID_RESULT",
            Self::ContextChanged => "MCP_MRTR_CONTEXT_CHANGED",
            Self::Cancelled => "MCP_MRTR_CANCELLED",
            Self::Timeout => "MCP_MRTR_TIMEOUT",
            Self::TransportFailed => "MCP_MRTR_TRANSPORT_FAILED",
            Self::TaskTimeout => "MCP_TASK_TIMEOUT",
            Self::TaskFailed => "MCP_TASK_FAILED",
            Self::TaskCancelled => "MCP_TASK_CANCELLED",
        }
    }
    pub const fn message(self) -> &'static str {
        match self {
            Self::RoundLimit => {
                "Request stopped after 8 input rounds; start a new request to continue."
            }
            Self::InputLimit => "Server input exceeds the client's supported limits.",
            Self::InvalidInput => "Server returned an invalid input request or form schema.",
            Self::UnsupportedInput => "Server requested an undeclared client feature.",
            Self::ProcessingUnavailable => "Elicitation processing unavailable.",
            Self::InvalidResult => "Server returned an unsupported or invalid result type/shape.",
            Self::ContextChanged => {
                "Request stopped because its session or server authorization changed."
            }
            Self::Cancelled => "Request was canceled before completion.",
            Self::Timeout => "Request exceeded its operation deadline.",
            Self::TransportFailed => {
                "Request could not complete; it was not automatically replayed."
            }
            Self::TaskTimeout => {
                "Task exceeded SENPI_MCP_TASK_TIMEOUT_MS; cancellation was requested."
            }
            Self::TaskFailed => "Task failed with a JSON-RPC error.",
            Self::TaskCancelled => "Task was cancelled by the server.",
        }
    }
    pub fn parse(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == code)
    }
}

/// Upstream `McpMrtrError`: `source = "local"`, never retriable.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct McpMrtrError {
    pub code: McpMrtrErrorCode,
    pub remote_code: Option<i64>,
    pub message: String,
}
impl McpMrtrError {
    pub const SOURCE: &'static str = "local";
    pub const RETRIABLE: bool = false;
    pub fn new(code: McpMrtrErrorCode) -> Self {
        Self::with(code, None, None)
    }
    pub fn with(code: McpMrtrErrorCode, remote_code: Option<i64>, task_id: Option<&str>) -> Self {
        let prefix = task_id.map_or_else(String::new, |id| format!("Task {id}: "));
        let suffix = remote_code.map_or_else(String::new, |code| format!(" (remote code {code})"));
        Self {
            code,
            remote_code,
            message: format!("{prefix}{}{suffix}", code.message()),
        }
    }
}
impl From<McpMrtrError> for crate::errors::McpError {
    fn from(error: McpMrtrError) -> Self {
        let mut mapped = Self::new(crate::errors::McpErrorKind::Protocol, error.message.clone());
        mapped.phase = Some("mrtr".into());
        mapped.retriable = McpMrtrError::RETRIABLE;
        mapped.cause = Some(Box::new(json!({
            "name": "McpMrtrError",
            "source": McpMrtrError::SOURCE,
            "code": error.code.as_str(),
            "remoteCode": error.remote_code,
        })));
        mapped
    }
}
/// Recover an MRTR failure that crossed an `McpError` boundary (upstream `instanceof McpMrtrError`).
pub fn as_mcp_mrtr_error(error: &crate::errors::McpError) -> Option<McpMrtrError> {
    let cause = error.cause.as_deref()?;
    if cause.get("name").and_then(Value::as_str) != Some("McpMrtrError") {
        return None;
    }
    let code = McpMrtrErrorCode::parse(cause.get("code").and_then(Value::as_str)?)?;
    Some(McpMrtrError {
        code,
        remote_code: cause.get("remoteCode").and_then(Value::as_i64),
        message: error.message.clone(),
    })
}
