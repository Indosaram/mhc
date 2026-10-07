use super::*;
use tempfile::tempdir;

#[test]
fn slot_paths_reject_a_non_positive_slot() {
    let dir = tempdir().expect("tempdir");
    assert!(recall_wake_lock_path(dir.path(), 0).is_err());
    assert_eq!(
        recall_wake_lock_path(dir.path(), 2).expect("slot path"),
        dir.path().join("recall-wake.slot-2.lock")
    );
    assert_eq!(
        recall_wake_ticket_directory(dir.path()),
        dir.path().join("recall-wake.tickets")
    );
}

#[test]
fn default_slots_admit_two_concurrent_leases_and_refuse_the_third() {
    let dir = tempdir().expect("tempdir");
    let first = acquire_recall_wake_lease(dir.path(), &RecallWakeLeaseOptions::default())
        .expect("first lease");
    let second = acquire_recall_wake_lease(dir.path(), &RecallWakeLeaseOptions::default())
        .expect("second lease");
    assert_eq!(first.slot, 1);
    assert_eq!(second.slot, 2);

    let third = acquire_recall_wake_lease(dir.path(), &RecallWakeLeaseOptions::default())
        .expect_err("third lease must be busy");
    match third {
        RecallWakeError::Busy(busy) => {
            assert_eq!(busy.max_concurrent, RECALL_WAKE_DEFAULT_SLOTS);
            assert!(busy.retriable());
        }
        other => panic!("expected busy, got {other:?}"),
    }

    assert!(first.release());
    let reclaimed = acquire_recall_wake_lease(dir.path(), &RecallWakeLeaseOptions::default())
        .expect("slot reclaimed after release");
    assert_eq!(reclaimed.slot, 1);
    reclaimed.release();
    second.release();
}

#[test]
fn an_aborted_caller_is_refused_and_leaves_no_ticket_behind() {
    let dir = tempdir().expect("tempdir");
    let aborted = || true;
    let result = acquire_recall_wake_lease(
        dir.path(),
        &RecallWakeLeaseOptions {
            cancellation: Some(&aborted),
            ..Default::default()
        },
    );
    assert!(matches!(result, Err(RecallWakeError::Aborted)));
    let tickets = list_tickets(&recall_wake_ticket_directory(dir.path())).expect("tickets");
    assert!(tickets.is_empty(), "aborted acquisition must withdraw its ticket");
}

#[test]
fn a_dead_head_ticket_is_reaped_so_the_queue_moves() {
    let dir = tempdir().expect("tempdir");
    let tickets = recall_wake_ticket_directory(dir.path());
    fs::create_dir_all(&tickets).expect("mkdir tickets");

    let mut record = create_lock_record(TICKET_PURPOSE, CreateLockRecordOptions::default())
        .expect("record");
    record.pid = 2_000_000_000;
    let head = "0000000000000001-000001-0000000001-11111111-2222-4333-8444-555555555555.ticket";
    publish_ticket(&tickets, head, &record).expect("publish head");

    assert!(reap_dead_head(&tickets, head).expect("reap"));
    assert!(list_tickets(&tickets).expect("tickets").is_empty());
}

#[test]
fn a_live_head_ticket_keeps_its_place() {
    let dir = tempdir().expect("tempdir");
    let tickets = recall_wake_ticket_directory(dir.path());
    fs::create_dir_all(&tickets).expect("mkdir tickets");
    let record = create_lock_record(TICKET_PURPOSE, CreateLockRecordOptions::default())
        .expect("record");
    let head = "0000000000000001-000001-0000000001-11111111-2222-4333-8444-555555555555.ticket";
    publish_ticket(&tickets, head, &record).expect("publish head");

    assert!(!reap_dead_head(&tickets, head).expect("reap"));
    assert_eq!(list_tickets(&tickets).expect("tickets").len(), 1);
}

#[test]
fn a_missing_head_ticket_is_treated_as_already_gone() {
    let dir = tempdir().expect("tempdir");
    let tickets = recall_wake_ticket_directory(dir.path());
    assert!(reap_dead_head(&tickets, "absent.ticket").expect("reap"));
}

#[test]
fn an_invalid_slot_count_is_refused_before_any_ticket_is_written() {
    let dir = tempdir().expect("tempdir");
    let result = acquire_recall_wake_lease(
        dir.path(),
        &RecallWakeLeaseOptions {
            max_concurrent: Some(0),
            ..Default::default()
        },
    );
    assert!(matches!(result, Err(RecallWakeError::InvalidOptions(_))));
    assert!(list_tickets(&recall_wake_ticket_directory(dir.path()))
        .expect("tickets")
        .is_empty());
}

#[test]
fn with_recall_wake_lease_releases_the_slot_after_the_operation() {
    let dir = tempdir().expect("tempdir");
    let slot = with_recall_wake_lease(dir.path(), &RecallWakeLeaseOptions::default(), |lease| lease.slot)
        .expect("lease");
    assert_eq!(slot, 1);
    let again = acquire_recall_wake_lease(dir.path(), &RecallWakeLeaseOptions::default())
        .expect("released slot is free");
    assert_eq!(again.slot, 1);
    again.release();
}

#[test]
fn try_release_releases_an_acquired_lease_and_frees_the_slot() {
    let dir = tempdir().expect("tempdir");
    let lease =
        acquire_recall_wake_lease(dir.path(), &RecallWakeLeaseOptions::default()).expect("lease");
    let lock_path = recall_wake_lock_path(dir.path(), lease.slot).expect("slot path");
    assert!(lock_path.exists());
    assert!(lease.try_release().expect("release reports ok(true)"));
    assert!(!lock_path.exists(), "a successful release removes the lock file");
    let reclaimed =
        acquire_recall_wake_lease(dir.path(), &RecallWakeLeaseOptions::default()).expect("released slot is free");
    assert_eq!(reclaimed.slot, 1);
    reclaimed.release();
}

#[test]
fn a_repeated_try_release_reports_already_gone() {
    let dir = tempdir().expect("tempdir");
    let lease =
        acquire_recall_wake_lease(dir.path(), &RecallWakeLeaseOptions::default()).expect("lease");
    assert!(lease.try_release().expect("first release reports ok(true)"));
    assert!(!lease.try_release().expect("second release reports ok(false)"));
    assert!(!lease.release(), "the retained bool API mirrors the already-gone result");
}

#[cfg(unix)]
#[test]
fn a_path_failure_surfaces_as_an_io_error_not_already_gone() {
    let dir = tempdir().expect("tempdir");
    // A regular file where a directory is needed: the release syscall fails with a path error
    // (ENOTDIR), deterministically, without relying on permissions, timing or a missing file.
    let blocker = dir.path().join("not-a-directory");
    std::fs::write(&blocker, b"x").expect("write blocker");
    let lease = RecallWakeLease {
        slot: 1,
        lock_path: blocker.join("recall-wake.slot-1.lock"),
        nonce: "nonce".to_string(),
    };
    let expected = std::fs::read_to_string(&lease.lock_path).expect_err("the probe read fails");
    let error = lease
        .try_release()
        .expect_err("a path failure is an io error, never the already-gone false");
    assert_eq!(error.kind(), expected.kind(), "the real io kind is preserved, not collapsed to false");
    assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
}
