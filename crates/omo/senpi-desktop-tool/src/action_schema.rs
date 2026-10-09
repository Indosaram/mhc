//! Action schema errors matching upstream `src/action-schema.ts`.

use serde::{Deserialize, Serialize};

/// Arguments that fit a tool's published root object but not the shape of the chosen action.
/// Providers reject a root-union JSON schema, so each computer tool publishes one root object
/// and checks the per-action shape at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("COMPUTER_INVALID_ARGUMENTS: {reason}")]
pub struct ComputerArgumentsError {
    pub code: String,
    pub reason: String,
}

impl ComputerArgumentsError {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            code: "COMPUTER_INVALID_ARGUMENTS".to_string(),
            reason: reason.into(),
        }
    }
}
