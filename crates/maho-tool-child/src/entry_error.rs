//! senpi `tool-child/entry-error.ts`.

/// A typed failure a tool-child job reports to the server as `job_error{code}`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ToolChildJobError {
    pub code: String,
    pub message: String,
}

impl ToolChildJobError {
    /// The upstream `this.name`.
    pub const NAME: &'static str = "ToolChildJobError";

    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn name(&self) -> &'static str {
        Self::NAME
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carries_code_message_and_name() {
        let error = ToolChildJobError::new("tool_failed", "boom");
        assert_eq!(error.code, "tool_failed");
        assert_eq!(error.to_string(), "boom");
        assert_eq!(error.name(), "ToolChildJobError");
    }
}
