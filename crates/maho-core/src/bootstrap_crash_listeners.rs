//! Port of senpi `packages/coding-agent/src/core/bootstrap-crash-listeners.ts`.
//!
//! Node keeps crash listeners on `process`; the Rust process has one panic hook. A mode that installs
//! its own crash handling (interactive restores the terminal first) marks the event here, and the
//! bootstrap containment defers only to marked handlers, so neither another bootstrap listener nor an
//! unrelated library hook can silence a real crash.

use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashEvent {
    UncaughtException,
    UnhandledRejection,
}

static UNCAUGHT: AtomicBool = AtomicBool::new(false);
static UNHANDLED: AtomicBool = AtomicBool::new(false);

fn slot(event: CrashEvent) -> &'static AtomicBool {
    match event {
        CrashEvent::UncaughtException => &UNCAUGHT,
        CrashEvent::UnhandledRejection => &UNHANDLED,
    }
}

/// Registers a mode's own crash handler for `event` (interactive); bootstrap listeners defer only to these.
pub fn mark_mode_crash_handler(event: CrashEvent) {
    slot(event).store(true, Ordering::SeqCst);
}

/// Removes a mode's mark when it uninstalls its handler.
pub fn unmark_mode_crash_handler(event: CrashEvent) {
    slot(event).store(false, Ordering::SeqCst);
}

/// True when a mode has registered its own handler for `event`.
pub fn has_mode_crash_handler(event: CrashEvent) -> bool {
    slot(event).load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_marked_events_count() {
        assert!(!has_mode_crash_handler(CrashEvent::UnhandledRejection));
        mark_mode_crash_handler(CrashEvent::UnhandledRejection);
        assert!(has_mode_crash_handler(CrashEvent::UnhandledRejection));
        unmark_mode_crash_handler(CrashEvent::UnhandledRejection);
        assert!(!has_mode_crash_handler(CrashEvent::UnhandledRejection));
    }
}
