//! Retired-extension-context detection (pin `stale-extension-context.ts`, #7946 and #10339).
//!
//! The runner guard throws these messages from a retired extension ctx: the first comes from session
//! replacement (new session, fork, switch) and the explicit reload path, the second from the
//! runtime's own reload, which invalidates the previous runner generation with a shorter message.

const STALE_EXTENSION_CONTEXT_ERROR_PREFIXES: [&str; 2] = [
    "This extension ctx is stale after session replacement or reload.",
    "stale extension generation after reload",
];

/// True when `message` is a retired-context guard error.
pub fn is_stale_extension_context_error(message: &str) -> bool {
    STALE_EXTENSION_CONTEXT_ERROR_PREFIXES
        .iter()
        .any(|prefix| message.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_runner_guard_prefixes_are_stale() {
        assert!(is_stale_extension_context_error(
            "This extension ctx is stale after session replacement or reload. Do not use it."
        ));
        assert!(is_stale_extension_context_error(
            "stale extension generation after reload"
        ));
    }

    #[test]
    fn other_errors_and_mid_message_matches_are_not_stale() {
        assert!(!is_stale_extension_context_error("boom"));
        assert!(!is_stale_extension_context_error(
            "wrapped: stale extension generation after reload"
        ));
    }
}
