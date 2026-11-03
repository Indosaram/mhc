//! Port of senpi packages/coding-agent/src/core/stale-extension-containment.ts.
//!
//! The runner's stale guard throws before a retired generation's operation runs, so the live session
//! must keep running: the stack is logged (it names the extension to fix) and the error stops there
//! (oh-my-openagent#7114). Upstream installs a process `uncaughtException` listener; Rust has no
//! process-level recoverable throw, so callers that catch a failure ask `contain_stale_extension_error`.

use crate::bootstrap_crash_listeners::CrashEvent;
use crate::hidden_stdout_log::append_debug_log_entry;

/// The stale-guard message prefix, shared with the goal extension's `stale_context`.
pub const STALE_EXTENSION_CONTEXT_ERROR_PREFIX: &str =
    "This extension ctx is stale after session replacement or reload.";

pub fn is_stale_extension_context_error(message: &str) -> bool {
    message.starts_with(STALE_EXTENSION_CONTEXT_ERROR_PREFIX)
}

/// True when the error was the stale-ctx guard and has been contained (logged, not rethrown).
pub fn contain_stale_extension_error(
    message: &str,
    stack: Option<&str>,
    origin: CrashEvent,
) -> bool {
    if !is_stale_extension_context_error(message) {
        return false;
    }
    let origin = match origin {
        CrashEvent::UncaughtException => "uncaughtException",
        CrashEvent::UnhandledRejection => "unhandledRejection",
    };
    append_debug_log_entry(
        &format!("contained stale extension ctx ({origin})"),
        stack.unwrap_or(message),
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_only_the_stale_context_error() {
        let stale = format!("{STALE_EXTENSION_CONTEXT_ERROR_PREFIX} retired");
        assert!(contain_stale_extension_error(
            &stale,
            None,
            CrashEvent::UncaughtException
        ));
        assert!(!contain_stale_extension_error(
            "boom",
            Some("at x"),
            CrashEvent::UnhandledRejection
        ));
    }
}
