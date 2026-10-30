//! The hook-to-worker handoff, independent of Win32 registration.
//!
//! mhc base: the stop is a `RegisterHotKey` `WM_HOTKEY` drained on the
//! listener thread (no `WH_KEYBOARD_LL` hook yet), so the listener requests
//! and completes in the same drain; the split still commits each requested
//! generation exactly once.
use std::sync::atomic::{AtomicBool, Ordering};

use senpi_desktop_safety::{StopSource, Supervisor};

#[derive(Default)]
pub(super) struct PendingStop(AtomicBool);

impl PendingStop {
    pub(super) fn request(&self, supervisor: &Supervisor) {
        supervisor.suspend_input();
        self.0.store(true, Ordering::SeqCst);
    }

    pub(super) fn complete(&self, supervisor: &Supervisor) -> bool {
        if !self.0.swap(false, Ordering::SeqCst) {
            return false;
        }
        supervisor.complete_stop(StopSource::Hotkey);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::PendingStop;
    use senpi_desktop_safety::{MonotonicClock, Supervisor};
    use std::sync::{Arc, Mutex};

    #[test]
    fn one_physical_hook_chord_commits_exactly_one_stop_generation() {
        // Given: the callback/worker handoff over an authority supervisor.
        let generations = Arc::new(Mutex::new(Vec::new()));
        let committed = Arc::clone(&generations);
        let supervisor =
            Supervisor::with_stop_authority(Arc::new(MonotonicClock::new()), move |generation| {
                committed.lock().unwrap().push(generation)
            });
        let pending = PendingStop::default();

        // When: one chord press reaches the listener.
        pending.request(&supervisor);
        assert!(supervisor.is_suspended());
        assert!(supervisor.stop_pending());
        assert!(generations.lock().unwrap().is_empty());
        assert!(pending.complete(&supervisor));

        // Then: the worker covers the press's generation, not a second request.
        assert_eq!(*generations.lock().unwrap(), [1]);
        assert!(!supervisor.stop_pending());
        assert!(!pending.complete(&supervisor));
        assert_eq!(*generations.lock().unwrap(), [1]);
    }
}
