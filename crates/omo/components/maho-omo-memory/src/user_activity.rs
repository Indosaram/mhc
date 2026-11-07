//! Which sessions this engine run has accepted input from (`user-activity.ts`).
//!
//! Permission is local to this engine run, never restored from transcript history.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};

#[derive(Default)]
pub struct MemoryUserActivity {
    accepted: Mutex<BTreeSet<String>>,
    reconciled: Mutex<BTreeSet<String>>,
    /// Sessions whose `reconcile_once` is running now (upstream `inFlight` promise map).
    in_flight: Mutex<BTreeMap<String, u64>>,
    next_task: std::sync::atomic::AtomicU64,
    inputs: Mutex<BTreeMap<String, String>>,
}

impl MemoryUserActivity {
    /// Whether the session was accepted (its input started or queued) in this run.
    pub fn has(&self, session_id: &str) -> bool {
        lock(&self.accepted).contains(session_id)
    }

    /// Whether session-start reconciliation already ran for this session. A later turn only recovers a
    /// reflection holder that died after that pass, so nothing is reclaimed before it.
    pub fn has_reconciled(&self, session_id: &str) -> bool {
        lock(&self.reconciled).contains(session_id)
    }

    pub fn has_input(&self, session_id: &str) -> bool {
        self.has(session_id) || lock(&self.inputs).values().any(|owner| owner == session_id)
    }

    pub fn reset(&self, session_id: &str) {
        lock(&self.accepted).remove(session_id);
        lock(&self.reconciled).remove(session_id);
        lock(&self.in_flight).remove(session_id);
        lock(&self.inputs).retain(|_, owner| owner != session_id);
    }

    /// Runs `reconcile` once per accepted session; a second settle in the same session is a no-op, and
    /// a caller arriving while another reconcile for the session is in flight does not start a second
    /// one. The session is marked reconciled only if this call is still the current task (a `reset`
    /// during the run discards it).
    pub fn reconcile_once(&self, session_id: &str, reconcile: impl FnOnce()) {
        if !self.has(session_id) || self.has_reconciled(session_id) {
            return;
        }
        let task = self
            .next_task
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut in_flight = lock(&self.in_flight);
            if in_flight.contains_key(session_id) {
                return;
            }
            in_flight.insert(session_id.to_string(), task);
        }
        let is_current = |activity: &Self| lock(&activity.in_flight).get(session_id) == Some(&task);
        if is_current(self) {
            reconcile();
        }
        if is_current(self) {
            lock(&self.reconciled).insert(session_id.to_string());
            lock(&self.in_flight).remove(session_id);
        }
    }

    /// Records that this session's initial reconciliation completed (`reconcileOnce`'s effect).
    pub fn mark_reconciled(&self, session_id: &str) {
        lock(&self.reconciled).insert(session_id.to_string());
    }

    /// Records that this session was accepted (its input started or queued) in this run.
    pub fn accept(&self, session_id: &str) {
        lock(&self.accepted).insert(session_id.to_string());
    }

    /// The host's `input` event: remembers which session an input id belongs to.
    pub fn record_input(&self, input_id: &str, session_id: &str) {
        lock(&self.inputs).insert(input_id.to_string(), session_id.to_string());
    }

    /// The host's `input_disposition` event: a started or queued input accepts its session.
    pub fn disposition(&self, input_id: &str, disposition: &str) {
        let owner = lock(&self.inputs).remove(input_id);
        if let Some(session_id) = owner
            && (disposition == "started" || disposition == "queued")
        {
            lock(&self.accepted).insert(session_id);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_accepted_session_reconciles_once_and_remembers_it() {
        let activity = MemoryUserActivity::default();
        assert!(!activity.has("session"));
        activity.record_input("input-1", "session");
        assert!(activity.has_input("session"));
        assert!(!activity.has("session"));
        activity.disposition("input-1", "started");
        assert!(activity.has("session"));
        assert!(!activity.has_reconciled("session"));
        let mut runs = 0;
        for _ in 0..2 {
            activity.reconcile_once("session", || runs += 1);
        }
        assert_eq!(runs, 1);
        assert!(activity.has_reconciled("session"));
    }

    #[test]
    fn a_queued_input_accepts_and_a_dropped_one_does_not() {
        let activity = MemoryUserActivity::default();
        activity.record_input("queued", "session");
        activity.disposition("queued", "queued");
        assert!(activity.has("session"));
        activity.reset("session");
        activity.record_input("dropped", "session");
        activity.disposition("dropped", "dropped");
        assert!(!activity.has("session"));
        assert!(!activity.has_input("session"));
    }

    #[test]
    fn reset_forgets_permission_and_reconciliation_for_that_session_only() {
        let activity = MemoryUserActivity::default();
        activity.record_input("a", "one");
        activity.disposition("a", "started");
        activity.reconcile_once("one", || {});
        activity.record_input("b", "two");
        activity.disposition("b", "started");
        activity.reset("one");
        assert!(!activity.has("one"));
        assert!(!activity.has_reconciled("one"));
        assert!(activity.has("two"));
        // A session that never reconciled is never reconciled by a later settle of another session.
        activity.reconcile_once("one", || panic!("not accepted"));
    }

    #[test]
    fn a_concurrent_caller_does_not_start_a_second_reconcile() {
        let activity = MemoryUserActivity::default();
        activity.accept("session");
        let mut inner_runs = 0;
        activity.reconcile_once("session", || {
            activity.reconcile_once("session", || inner_runs += 1);
        });
        assert_eq!(inner_runs, 0);
        assert!(activity.has_reconciled("session"));
    }

    #[test]
    fn a_reset_during_the_run_discards_the_reconciled_mark() {
        let activity = MemoryUserActivity::default();
        activity.accept("session");
        activity.reconcile_once("session", || activity.reset("session"));
        assert!(!activity.has_reconciled("session"));
    }
}
