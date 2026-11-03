//! `runners/rpc-host/session-wire.ts` (v5.1.52 additions): typed input refusals of a hosted session.

use crate::host::HostError;

/// `HostSessionUnresponsiveError`: nothing was sent; the same session can accept input once its
/// state probes recover.
pub const HOST_SESSION_UNRESPONSIVE_CODE: &str = "unresponsive";
pub const HOST_SESSION_UNRESPONSIVE_MESSAGE: &str =
    "the session is unresponsive; input can be retried when it responds";

pub fn host_session_unresponsive_error() -> HostError {
    HostError {
        message: HOST_SESSION_UNRESPONSIVE_MESSAGE.to_string(),
    }
}

/// Whether an input failure is the retryable "unresponsive" refusal.
pub fn is_host_session_unresponsive(error: &HostError) -> bool {
    error.message == HOST_SESSION_UNRESPONSIVE_MESSAGE
}

/// `isHostSessionMissing`: the engine's typed response, including hosts that carry the code only in
/// the error string.
pub fn is_host_session_missing(error_code: Option<&str>, message: &str) -> bool {
    error_code == Some("unknown_session") || message == "unknown_session"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_session_is_read_from_the_code_or_the_bare_message() {
        assert!(is_host_session_missing(Some("unknown_session"), "gone"));
        assert!(is_host_session_missing(None, "unknown_session"));
        assert!(!is_host_session_missing(None, "unknown_session: other"));
        assert!(is_host_session_unresponsive(
            &host_session_unresponsive_error()
        ));
    }
}
