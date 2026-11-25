//! Port of senpi `packages/coding-agent/src/core/required-compaction-error.ts`.

use std::fmt;

/// The error a turn ends with when a required compaction could not bring the context back under the window.
/// It names no slash command, because print mode, RPC and SDK callers show it as is; the TUI swaps in its own wording.
pub const REQUIRED_COMPACTION_ERROR_MESSAGE: &str = "Context remains above the compaction threshold because compaction did not complete. Retry the prompt to run compaction again, or start a new session.";

pub const REQUIRED_COMPACTION_ERROR_CODE: &str = "compaction_failed";

/// A required compaction did not complete; `code` lets SDK callers branch on it without matching the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredCompactionError;

impl RequiredCompactionError {
    pub fn code(&self) -> &'static str {
        REQUIRED_COMPACTION_ERROR_CODE
    }
}

impl fmt::Display for RequiredCompactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REQUIRED_COMPACTION_ERROR_MESSAGE)
    }
}

impl std::error::Error for RequiredCompactionError {}
