use serde_json::Value;
use crate::errors::{has_status_word, McpError, McpErrorKind};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthFailureKind { S256Unsupported, NeedsAuth, InvalidGrant, Transient, StateMismatch, ExpiredCode, NoVerifier, Headless, MissingEnv }
#[derive(Debug, Clone, thiserror::Error)]
#[error("{error}")]
pub struct OAuthFlowError { pub oauth_kind: OAuthFailureKind, pub terminal: bool, pub error: McpError }
impl OAuthFlowError {
    pub fn new(kind: OAuthFailureKind, message: impl Into<String>) -> Self { Self { oauth_kind: kind, terminal: kind != OAuthFailureKind::Transient, error: McpError::new(McpErrorKind::Auth,message) } }
}
pub fn is_invalid_grant(error: &Value) -> bool {
    if let Some(code) = error.get("errorCode").and_then(Value::as_str) { return ["invalid_grant","invalid_token","invalid_client","unauthorized_client"].contains(&code); }
    let text = error_text(error).to_lowercase(); text.contains("invalid_grant") || text.contains("invalid_token")
}
pub fn is_transient_token_error(error: &Value) -> bool {
    if is_invalid_grant(error) { return false; }
    if error.get("errorCode").and_then(Value::as_str).is_some_and(|code| ["temporarily_unavailable","server_error","slow_down"].contains(&code)) { return true; }
    let text = error_text(error).to_lowercase(); has_status_word(&text,&["500","502","503","504"]) || text.contains("econnrefused") || text.contains("network")
}
fn error_text(error: &Value) -> String { match error { Value::String(s) => s.clone(), Value::Object(_) => format!("{} {}",error.get("name").and_then(Value::as_str).unwrap_or("Error"),error.get("message").and_then(Value::as_str).unwrap_or("")), _ => error.to_string() } }
