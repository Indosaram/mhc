//! Typed refusal when a child's engine does not hold its designated model chain (`#12759`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesignatedChainNotHeldReason {
    Unsupported,
    Refused,
    Timeout,
}

impl DesignatedChainNotHeldReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Refused => "refused",
            Self::Timeout => "timeout",
        }
    }
}

/// `models` comes from the child's own spec (omo-computed), never from engine output.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct DesignatedChainNotHeldError {
    pub message: String,
    pub reason: DesignatedChainNotHeldReason,
    pub models: Vec<String>,
}
