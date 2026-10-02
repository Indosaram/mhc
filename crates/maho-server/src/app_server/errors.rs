pub use super::registry::JsonRpcError;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NonSteerableTurnKind {
    Review,
    Compact,
}
#[derive(Debug, Clone, PartialEq)]
pub enum CodexErrorInfo {
    ContextWindowExceeded,
    SessionBudgetExceeded,
    UsageLimitExceeded,
    ServerOverloaded,
    CyberPolicy,
    HttpConnectionFailed(Option<f64>),
    ResponseStreamConnectionFailed(Option<f64>),
    InternalServerError,
    Unauthorized,
    BadRequest,
    ThreadRollbackFailed,
    SandboxError,
    ResponseStreamDisconnected(Option<f64>),
    ResponseTooManyFailedAttempts(Option<f64>),
    ActiveTurnNotSteerable(NonSteerableTurnKind),
    Other,
}
pub fn serialize_codex_error_info(info: &CodexErrorInfo) -> Value {
    match info {
        CodexErrorInfo::ContextWindowExceeded => json!("contextWindowExceeded"),
        CodexErrorInfo::SessionBudgetExceeded => json!("sessionBudgetExceeded"),
        CodexErrorInfo::UsageLimitExceeded => json!("usageLimitExceeded"),
        CodexErrorInfo::ServerOverloaded => json!("serverOverloaded"),
        CodexErrorInfo::CyberPolicy => json!("cyberPolicy"),
        CodexErrorInfo::InternalServerError => json!("internalServerError"),
        CodexErrorInfo::Unauthorized => json!("unauthorized"),
        CodexErrorInfo::BadRequest => json!("badRequest"),
        CodexErrorInfo::ThreadRollbackFailed => json!("threadRollbackFailed"),
        CodexErrorInfo::SandboxError => json!("sandboxError"),
        CodexErrorInfo::Other => json!("other"),
        CodexErrorInfo::HttpConnectionFailed(code) => {
            json!({"httpConnectionFailed":{"httpStatusCode":code}})
        }
        CodexErrorInfo::ResponseStreamConnectionFailed(code) => {
            json!({"responseStreamConnectionFailed":{"httpStatusCode":code}})
        }
        CodexErrorInfo::ResponseStreamDisconnected(code) => {
            json!({"responseStreamDisconnected":{"httpStatusCode":code}})
        }
        CodexErrorInfo::ResponseTooManyFailedAttempts(code) => {
            json!({"responseTooManyFailedAttempts":{"httpStatusCode":code}})
        }
        CodexErrorInfo::ActiveTurnNotSteerable(kind) => {
            json!({"activeTurnNotSteerable":{"turnKind":kind}})
        }
    }
}
pub fn parse_error() -> JsonRpcError {
    JsonRpcError::new(-32700, "Parse error")
}
pub fn invalid_request_error() -> JsonRpcError {
    JsonRpcError::new(-32600, "Invalid request")
}
pub fn method_not_found_error(method: &str) -> JsonRpcError {
    JsonRpcError::new(-32601, format!("Method not found: {method}"))
}
pub fn invalid_params_error() -> JsonRpcError {
    JsonRpcError::new(-32602, "Invalid params")
}
pub fn internal_error(message: Option<&str>) -> JsonRpcError {
    JsonRpcError::new(-32603, message.unwrap_or("Internal error"))
}
pub fn overloaded_error() -> JsonRpcError {
    JsonRpcError::new(-32001, "Server overloaded; retry later.")
}
pub fn not_initialized_error() -> JsonRpcError {
    JsonRpcError::new(-32600, "Not initialized")
}
pub fn already_initialized_error() -> JsonRpcError {
    JsonRpcError::new(-32600, "Already initialized")
}
pub fn experimental_capability_error(method: &str) -> JsonRpcError {
    JsonRpcError::new(
        -32600,
        format!("{method} requires experimentalApi capability"),
    )
}
