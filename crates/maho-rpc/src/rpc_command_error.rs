//! Port of senpi `packages/coding-agent/src/modes/rpc/rpc-command-error.ts`.
//!
//! A command the host refused; `error_code` carries the typed code when the command defines one.

use serde_json::Value;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct RpcCommandError {
    pub message: String,
    pub error_code: Option<String>,
    pub error_data: Option<Value>,
}

impl RpcCommandError {
    pub fn new(
        message: impl Into<String>,
        error_code: Option<String>,
        error_data: Option<Value>,
    ) -> Self {
        Self {
            message: message.into(),
            error_code,
            error_data,
        }
    }
}

impl fmt::Display for RpcCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RpcCommandError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preserves_error_code_and_data() {
        let err = RpcCommandError::new(
            "bad request",
            Some("invalid_argument".to_owned()),
            Some(json!({"field": "cwd"})),
        );
        assert_eq!(err.to_string(), "bad request");
        assert_eq!(err.error_code.as_deref(), Some("invalid_argument"));
        assert_eq!(err.error_data, Some(json!({"field": "cwd"})));
    }
}
