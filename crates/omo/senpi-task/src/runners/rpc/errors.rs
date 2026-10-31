//! `runners/rpc/errors.ts`.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

/// The shape an engine error code must have to leave the wire boundary; anything else may be free text.
pub static ERROR_CODE_SHAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_]{0,47}$").expect("error code shape"));
/// The shape a provider id must have to leave the wire boundary.
pub static PROVIDER_SHAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9._-]{0,63}$").expect("provider shape"));

/// Only the validated provider of the engine's `errorData`; the rest of the payload is never kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcRefusalData {
    pub provider: String,
}

/// A senpi RPC command that returned `success: false`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("RPC command {command} failed: {detail}")]
pub struct RpcCommandError {
    pub command: String,
    pub detail: String,
    pub error_code: Option<String>,
    pub error_data: Option<RpcRefusalData>,
}

impl RpcCommandError {
    pub fn new(command: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            detail: detail.into(),
            error_code: None,
            error_data: None,
        }
    }

    /// `new RpcCommandError(command, detail, refusal)`: keeps the refused response's typed fields,
    /// dropping anything off-shape at the wire boundary.
    pub fn with_refusal(
        command: impl Into<String>,
        detail: impl Into<String>,
        refusal: &Value,
    ) -> Self {
        let error_code = refusal
            .get("errorCode")
            .and_then(Value::as_str)
            .filter(|code| ERROR_CODE_SHAPE.is_match(code))
            .map(str::to_string);
        let error_data = refusal
            .get("errorData")
            .and_then(|data| data.get("provider"))
            .and_then(Value::as_str)
            .filter(|provider| PROVIDER_SHAPE.is_match(provider))
            .map(|provider| RpcRefusalData {
                provider: provider.to_string(),
            });
        Self {
            command: command.into(),
            detail: detail.into(),
            error_code,
            error_data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn refusal_keeps_only_shape_checked_code_and_provider() {
        let typed = RpcCommandError::with_refusal(
            "prompt",
            "No API key found for openai.",
            &json!({"success": false, "errorCode": "provider_credentials_missing", "errorData": {"provider": "openai", "secret": "sk-live"}}),
        );
        assert_eq!(
            typed.error_code.as_deref(),
            Some("provider_credentials_missing")
        );
        assert_eq!(
            typed.error_data,
            Some(RpcRefusalData {
                provider: "openai".into()
            })
        );
        let hostile = RpcCommandError::with_refusal(
            "prompt",
            "x",
            &json!({"errorCode": "Free Text with spaces", "errorData": {"provider": "../etc"}}),
        );
        assert_eq!(hostile.error_code, None);
        assert_eq!(hostile.error_data, None);
        assert_eq!(hostile.to_string(), "RPC command prompt failed: x");
    }
}
