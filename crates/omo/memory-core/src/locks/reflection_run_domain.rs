//! Reflection runs queue without a timeout; only execution consumes the run budget.
use super::counting_lease::{
    CountingLease, CountingLeaseDomain, CountingLeaseError, CountingLeaseOptions,
    acquire_counting_lease,
};
use std::path::Path;

pub fn acquire_reflection_run_lease(
    locks_directory: &Path,
    max_concurrent: Option<usize>,
    cancellation: Option<&dyn Fn() -> bool>,
) -> Result<CountingLease, CountingLeaseError> {
    acquire_counting_lease(
        locks_directory,
        &CountingLeaseDomain {
            purpose: "reflection-run",
            default_slots: 1,
            wait_timeout_ms: None,
        },
        &CountingLeaseOptions {
            max_concurrent,
            cancellation,
            ..Default::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflection_uses_its_own_single_slot_domain() {
        let directory = tempfile::tempdir().expect("directory");
        let lease = acquire_reflection_run_lease(directory.path(), None, None).expect("lease");
        assert_eq!(lease.slot, 1);
        assert!(directory.path().join("reflection-run.slot-1.lock").exists());
        let recall = crate::locks::acquire_recall_wake_lease(directory.path(), &Default::default())
            .expect("independent recall slot");
        assert_eq!(recall.slot, 1);
        assert!(lease.release_sync().expect("release"));
        assert!(recall.release());
    }

    #[test]
    fn cancelled_unbounded_queue_withdraws_ticket() {
        let directory = tempfile::tempdir().expect("directory");
        let cancelled = || true;
        assert!(matches!(
            acquire_reflection_run_lease(directory.path(), None, Some(&cancelled)),
            Err(CountingLeaseError::Aborted)
        ));
    }
}
