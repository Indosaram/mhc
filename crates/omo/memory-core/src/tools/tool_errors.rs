//! Typed memory tool errors matching TypeScript error envelopes.

use std::fmt;

/// Error returned by memory tool operations with human-readable error messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryToolError {
    pub message: String,
}

impl MemoryToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for MemoryToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for MemoryToolError {}
