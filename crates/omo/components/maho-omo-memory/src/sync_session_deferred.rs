//! The device session cannot be resolved now for a reason that clears without a configuration
//! change: the OS credential store is waiting for a person to answer its approval dialog (#11262).
//! Port of `sync-session-deferred.ts`.
//!
//! Resolver errors cross the runner as strings, so the code travels as a stable prefix of the message
//! (upstream recognizes the error by its `code`, not its class, for the same reason).
//!
//! Cross-lane dependency: nothing in the native OS credential store raises a pending keychain yet;
//! a store reports it with [`keychain_approval_pending`].

use std::fmt;

pub const SYNC_SESSION_DEFERRED: &str = "sync-session-deferred";
const DEFERRED_PREFIX: &str = "sync-session-deferred: ";
/// Marks a credential-store read that is parked behind the OS approval dialog (`KeychainApprovalPendingError`).
const KEYCHAIN_PENDING_PREFIX: &str = "keychain-approval-pending: ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSessionDeferredError {
    pub message: String,
}

impl SyncSessionDeferredError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        SYNC_SESSION_DEFERRED
    }
}

impl fmt::Display for SyncSessionDeferredError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{DEFERRED_PREFIX}{}", self.message)
    }
}

impl std::error::Error for SyncSessionDeferredError {}

/// `isSyncSessionDeferred` for an error that crossed as text: the deferral's message, or `None`.
pub fn is_sync_session_deferred(error: &str) -> Option<&str> {
    error.strip_prefix(DEFERRED_PREFIX)
}

/// A credential-store error meaning "waiting for the keychain approval dialog".
pub fn keychain_approval_pending(message: &str) -> String {
    format!("{KEYCHAIN_PENDING_PREFIX}{message}")
}

/// The message of a pending-keychain error, or `None` for any other error.
pub fn is_keychain_approval_pending(error: &str) -> Option<&str> {
    error.strip_prefix(KEYCHAIN_PENDING_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deferral_is_recognized_by_its_code_prefix() {
        let error = SyncSessionDeferredError::new("Waiting for the keychain.");
        assert_eq!(error.code(), SYNC_SESSION_DEFERRED);
        assert_eq!(
            is_sync_session_deferred(&error.to_string()),
            Some("Waiting for the keychain.")
        );
        assert_eq!(is_sync_session_deferred("another failure"), None);
    }

    #[test]
    fn a_pending_keychain_round_trips() {
        let pending = keychain_approval_pending("approve the dialog");
        assert_eq!(
            is_keychain_approval_pending(&pending),
            Some("approve the dialog")
        );
        assert_eq!(is_keychain_approval_pending("not signed in"), None);
    }
}
