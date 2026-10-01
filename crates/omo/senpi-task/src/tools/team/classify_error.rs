//! Port of `tools/team/classify-error.ts`.
//!
//! Discriminates the team-mailbox send failures the tool layer maps to structured results. team-core's
//! mailbox errors carry a stable name, so the tool layer keys on the name rather than importing every
//! error type (keeps the tools free of a direct team-mailbox dependency).

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MailboxErrorKind {
    RecipientBackpressure,
    InvalidRecipient,
    PayloadTooLarge,
    BroadcastDenied,
    TeamDeleting,
}

impl MailboxErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RecipientBackpressure => "recipient_backpressure",
            Self::InvalidRecipient => "invalid_recipient",
            Self::PayloadTooLarge => "payload_too_large",
            Self::BroadcastDenied => "broadcast_denied",
            Self::TeamDeleting => "team_deleting",
        }
    }
}

/// Errors that expose a stable JS-style `.name`.
pub trait NamedError {
    fn error_name(&self) -> &str;
}

pub fn classify_mailbox_error_name(name: &str) -> Option<MailboxErrorKind> {
    match name {
        "RecipientBackpressureError" => Some(MailboxErrorKind::RecipientBackpressure),
        "InvalidRecipientError" => Some(MailboxErrorKind::InvalidRecipient),
        "PayloadTooLargeError" => Some(MailboxErrorKind::PayloadTooLarge),
        "BroadcastNotPermittedError" => Some(MailboxErrorKind::BroadcastDenied),
        "TeamDeletingError" => Some(MailboxErrorKind::TeamDeleting),
        _ => None,
    }
}

pub fn classify_mailbox_error(error: Option<&dyn NamedError>) -> Option<MailboxErrorKind> {
    classify_mailbox_error_name(error?.error_name())
}

/// Equivalent of checking `error.code === "ENOENT"`.
pub fn is_missing_state_error(error: Option<&std::io::Error>) -> bool {
    match error {
        Some(error) => {
            error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ENOENT)
        }
        None => false,
    }
}
