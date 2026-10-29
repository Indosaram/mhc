use super::*;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

const UUID: &str = "11111111-2222-4333-8444-555555555555";

#[test]
fn candidate_name_shape_requires_the_uuid_suffix() {
    assert!(is_leaked_candidate_name(&format!(
        "writer.lock.candidate-{UUID}"
    )));
    assert!(!is_leaked_candidate_name("writer.lock"));
    assert!(!is_leaked_candidate_name("writer.lock.candidate-not-a-uuid"));
    assert!(!is_leaked_candidate_name("domain.candidate-run-7"));
}

#[test]
fn sweeps_a_stale_candidate_and_leaves_live_locks_alone() {
    let dir = tempdir().expect("tempdir");
    let candidate = dir.path().join(format!("writer.lock.candidate-{UUID}"));
    fs::write(&candidate, "leaked").expect("write candidate");
    fs::write(dir.path().join("writer.lock"), "live").expect("write live lock");

    let swept =
        sweep_stale_lock_candidates(dir.path(), || u64::MAX, &CandidateSweepOptions::default())
            .expect("sweep");

    assert_eq!(swept, 1);
    assert!(!candidate.exists());
    assert!(dir.path().join("writer.lock").exists());
}

#[test]
fn keeps_a_fresh_candidate_unless_it_is_tracked() {
    let dir = tempdir().expect("tempdir");
    let candidate = dir.path().join(format!("writer.lock.candidate-{UUID}"));
    fs::write(&candidate, "fresh").expect("write");

    let swept = sweep_stale_lock_candidates(dir.path(), || 0, &CandidateSweepOptions::default())
        .expect("sweep fresh");
    assert_eq!(swept, 0);
    assert!(candidate.exists());

    track_leaked_candidate(&candidate);
    let swept = sweep_stale_lock_candidates(dir.path(), || 0, &CandidateSweepOptions::default())
        .expect("sweep tracked");
    assert_eq!(swept, 1);
    assert!(!candidate.exists());
}

#[test]
fn missing_directory_reports_zero_and_forgets_tracked_candidates() {
    let dir = tempdir().expect("tempdir");
    let missing = dir.path().join("absent");
    let tracked = missing.join(format!("writer.lock.candidate-{UUID}"));
    track_leaked_candidate(&tracked);

    let swept = sweep_stale_lock_candidates(&missing, || 0, &CandidateSweepOptions::default())
        .expect("sweep missing");

    assert_eq!(swept, 0);
    assert!(!is_known_leaked(&tracked));
}

#[test]
fn a_sharing_error_retries_then_reports_failure() {
    let dir = tempdir().expect("tempdir");
    let candidate = dir.path().join(format!("writer.lock.candidate-{UUID}"));
    fs::write(&candidate, "held").expect("write");

    let attempts = std::cell::Cell::new(0usize);
    let failures = std::cell::Cell::new(0usize);
    let unlink = |_: &Path| -> std::io::Result<()> {
        attempts.set(attempts.get() + 1);
        Err(std::io::Error::from_raw_os_error(16))
    };
    let sharing = |_: &std::io::Error| true;
    let on_failure = |_: &Path| failures.set(failures.get() + 1);
    let options = CandidateSweepOptions {
        unlink: Some(&unlink),
        is_sharing_error: Some(&sharing),
        on_failure: Some(&on_failure),
    };

    let swept =
        sweep_stale_lock_candidates(dir.path(), || u64::MAX, &options).expect("sweep held");

    assert_eq!(swept, 0);
    assert_eq!(attempts.get(), CANDIDATE_UNLINK_ATTEMPTS);
    assert_eq!(failures.get(), 1);
    assert!(candidate.exists());
}
