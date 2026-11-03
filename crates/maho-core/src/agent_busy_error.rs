//! Port of senpi `packages/coding-agent/src/core/agent-busy-error.ts`.

/// `prompt` without a streaming behavior while a turn runs; `code` is the RPC `streaming` error code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct AgentBusyError {
    pub message: String,
}

impl AgentBusyError {
    pub const CODE: &'static str = "streaming";
    pub const NAME: &'static str = "AgentBusyError";

    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        Self::CODE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carries_the_streaming_code_and_message() {
        let error = AgentBusyError::new("busy");
        assert_eq!(error.to_string(), "busy");
        assert_eq!(error.code(), "streaming");
        assert_eq!(AgentBusyError::NAME, "AgentBusyError");
    }
}
