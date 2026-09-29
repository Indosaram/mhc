//! `lifecycle/admission-lease.test.ts`.
//!
//! The TS file drives renewal with jest fake timers. Lease staleness *is* wall-clock behaviour,
//! so these translations use real time with the same small timings: the holder's renew thread
//! must keep the lease fresh across several stale windows, and a crashed holder (renew far in the
//! future) must go stale.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::lifecycle::admission_lease::{
    AcquireAdmissionLeaseResult, AdmissionLeaseTimingOverrides, SessionAdmissionLease,
    acquire_session_admission_lease, admission_lease_path,
};

fn timing(
    renew: u64,
    stale: Option<u64>,
    timeout: Option<u64>,
    retry: Option<u64>,
) -> AdmissionLeaseTimingOverrides {
    AdmissionLeaseTimingOverrides {
        renew_ms: Some(renew),
        stale_ms: stale,
        acquire_timeout_ms: timeout,
        retry_ms: retry,
    }
}

fn acquire(
    dir: &std::path::Path,
    overrides: &AdmissionLeaseTimingOverrides,
) -> AcquireAdmissionLeaseResult {
    acquire_session_admission_lease(dir, "parent-1", *overrides).expect("acquire")
}

fn acquired(result: AcquireAdmissionLeaseResult) -> Arc<dyn SessionAdmissionLease> {
    match result {
        AcquireAdmissionLeaseResult::Acquired(lease) => lease,
        AcquireAdmissionLeaseResult::Contended => panic!("expected acquired, got contended"),
    }
}

fn body(path: &std::path::Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("lease body")).expect("lease json")
}

#[test]
fn acquire_writes_token_and_release_removes_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lease = acquired(acquire(dir.path(), &timing(40, None, None, None)));
    assert_eq!(lease.path(), admission_lease_path(dir.path(), "parent-1"));
    assert!(lease.path().exists());
    let body = body(lease.path());
    assert_eq!(body["token"].as_str(), Some(lease.token()));
    assert_eq!(body["pid"].as_u64(), Some(u64::from(std::process::id())));
    assert!(body["renewed_at"].is_number());
    lease.release();
    assert!(!lease.path().exists());
    acquired(acquire(dir.path(), &timing(40, None, None, None))).release();
}

#[test]
fn renewing_holder_is_not_reclaimed_after_several_stale_windows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let overrides = timing(40, Some(120), Some(0), Some(10));
    let holder = acquired(acquire(dir.path(), &overrides));
    std::thread::sleep(Duration::from_millis(400));
    let waiter = acquire(dir.path(), &overrides);
    assert!(matches!(waiter, AcquireAdmissionLeaseResult::Contended));
    assert!(holder.is_owner());
    holder.release();
}

#[test]
fn stale_crashed_holder_is_taken_over_and_its_late_release_is_harmless() {
    let dir = tempfile::tempdir().expect("tempdir");
    let crashed = acquired(acquire(
        dir.path(),
        &timing(60_000, Some(120), Some(300), Some(10)),
    ));
    let successor = acquired(acquire(
        dir.path(),
        &timing(40, Some(120), Some(2_000), Some(10)),
    ));
    assert_ne!(successor.token(), crashed.token());
    assert!(!crashed.is_owner());
    assert!(successor.is_owner());
    crashed.release();
    assert!(successor.is_owner());
    assert!(admission_lease_path(dir.path(), "parent-1").exists());
    successor.release();
}

#[test]
fn racing_takeover_has_exactly_one_winner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let crashed = acquired(acquire(
        dir.path(),
        &timing(60_000, Some(150), Some(300), Some(10)),
    ));
    std::thread::sleep(Duration::from_millis(200));
    let overrides = timing(50, Some(150), Some(1_500), Some(5));
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let racers: Vec<_> = (0..2)
        .map(|_| {
            let dir = dir.path().to_path_buf();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                acquire(&dir, &overrides)
            })
        })
        .collect();
    let results: Vec<_> = racers
        .into_iter()
        .map(|racer| racer.join().expect("racer"))
        .collect();
    let winners: Vec<_> = results
        .into_iter()
        .filter_map(|result| match result {
            AcquireAdmissionLeaseResult::Acquired(lease) => Some(lease),
            AcquireAdmissionLeaseResult::Contended => None,
        })
        .collect();
    assert_eq!(winners.len(), 1);
    let winner = &winners[0];
    assert!(winner.is_owner());
    crashed.release();
    assert!(winner.is_owner());
    let path = admission_lease_path(dir.path(), "parent-1");
    assert!(path.exists());
    assert_eq!(body(&path)["token"].as_str(), Some(winner.token()));
    winner.release();
    assert!(!path.exists());
}
