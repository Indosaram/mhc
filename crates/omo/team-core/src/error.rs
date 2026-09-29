//! The crate error type: one variant per TypeScript error class callers match on.

use std::io;

use crate::types::{RuntimeStatus, TaskStatus};

/// Every failure surfaced by team-core. Variant names mirror the TypeScript error classes;
/// [`TeamCoreError::name`] returns the original class name.
#[derive(Debug, thiserror::Error)]
pub enum TeamCoreError {
    #[error("team path escapes base directory")]
    TeamPathTraversal,
    #[error("{message}")]
    TeamSpecValidation {
        message: String,
        code: String,
        field: Option<String>,
        member_name: Option<String>,
    },
    #[error("{message}")]
    MemberValidation {
        message: String,
        member_name: Option<String>,
        issue: Option<String>,
    },
    #[error("{message}")]
    RuntimeState { message: String, code: String },
    #[error("invalid transition {from} -> {to}")]
    InvalidTransition {
        from: RuntimeStatus,
        to: RuntimeStatus,
    },
    #[error("broadcast requires lead role")]
    BroadcastNotPermitted,
    #[error("payload exceeds 32 KB")]
    PayloadTooLarge,
    #[error("recipient inbox full (backpressure)")]
    RecipientBackpressure,
    #[error("duplicate message id")]
    DuplicateMessageId,
    #[error("unknown or inactive team recipient: {0}")]
    InvalidRecipient(String),
    #[error("team is deleting")]
    TeamDeleting,
    #[error("already_claimed")]
    AlreadyClaimed,
    #[error("blocked by {}", .0.join(","))]
    BlockedBy(Vec<String>),
    #[error("no reverse transitions from {from} to {to}")]
    InvalidTaskTransition { from: TaskStatus, to: TaskStatus },
    #[error("cross-owner updates are not allowed")]
    CrossOwnerUpdate,
    #[error("git required for worktree members")]
    GitUnavailable,
    #[error("Timed out acquiring lock: {0}")]
    LockTimeout(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    /// A plain `Error` in the TypeScript original.
    #[error("{0}")]
    Message(String),
}

impl TeamCoreError {
    pub(crate) fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }

    pub(crate) fn spec_validation(
        message: impl Into<String>,
        code: &str,
        field: Option<&str>,
        member_name: Option<&str>,
    ) -> Self {
        Self::TeamSpecValidation {
            message: message.into(),
            code: code.to_owned(),
            field: field.map(str::to_owned),
            member_name: member_name.map(str::to_owned),
        }
    }

    /// The TypeScript error class name (`error.name`).
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::TeamPathTraversal => "TeamPathTraversalError",
            Self::TeamSpecValidation { .. } => "TeamSpecValidationError",
            Self::MemberValidation { .. } => "MemberValidationError",
            Self::RuntimeState { .. } => "RuntimeStateError",
            Self::InvalidTransition { .. } => "InvalidTransitionError",
            Self::BroadcastNotPermitted => "BroadcastNotPermittedError",
            Self::PayloadTooLarge => "PayloadTooLargeError",
            Self::RecipientBackpressure => "RecipientBackpressureError",
            Self::DuplicateMessageId => "DuplicateMessageIdError",
            Self::InvalidRecipient(_) => "InvalidRecipientError",
            Self::TeamDeleting => "TeamDeletingError",
            Self::AlreadyClaimed => "AlreadyClaimedError",
            Self::BlockedBy(_) => "BlockedByError",
            Self::InvalidTaskTransition { .. } => "InvalidTaskTransitionError",
            Self::CrossOwnerUpdate => "CrossOwnerUpdateError",
            Self::GitUnavailable => "GitUnavailableError",
            Self::LockTimeout(_) | Self::Io(_) | Self::Message(_) => "Error",
        }
    }

    /// The `code` field of errors that carry one.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::TeamSpecValidation { code, .. } | Self::RuntimeState { code, .. } => Some(code),
            _ => None,
        }
    }

    /// True for an I/O error whose kind is `NotFound` (Node's `ENOENT`).
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == io::ErrorKind::NotFound)
    }
}

pub type Result<T, E = TeamCoreError> = std::result::Result<T, E>;
