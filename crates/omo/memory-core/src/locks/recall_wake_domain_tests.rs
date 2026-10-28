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
    assert!(
        tickets.is_empty(),
        "aborted acquisition must withdraw its ticket"
    );
}

#[test]
fn a_dead_head_ticket_is_reaped_so_the_queue_moves() {
    let dir = tempdir().expect("tempdir");
    let tickets = recall_wake_ticket_directory(dir.path());
    fs::create_dir_all(&tickets).expect("mkdir tickets");

    let mut record =
        create_lock_record(TICKET_PURPOSE, CreateLockRecordOptions::default()).expect("record");
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
    let record =
        create_lock_record(TICKET_PURPOSE, CreateLockRecordOptions::default()).expect("record");
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
    assert!(
        list_tickets(&recall_wake_ticket_directory(dir.path()))
            .expect("tickets")
            .is_empty()
    );
}

#[test]
fn with_recall_wake_lease_releases_the_slot_after_the_operation() {
    let dir = tempdir().expect("tempdir");
    let slot = with_recall_wake_lease(dir.path(), &RecallWakeLeaseOptions::default(), |lease| {
        lease.slot
    })
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
    assert!(
        !lock_path.exists(),
        "a successful release removes the lock file"
    );
    let reclaimed = acquire_recall_wake_lease(dir.path(), &RecallWakeLeaseOptions::default())
        .expect("released slot is free");
    assert_eq!(reclaimed.slot, 1);
    reclaimed.release();
}

#[test]
fn a_repeated_try_release_reports_already_gone() {
    let dir = tempdir().expect("tempdir");
    let lease =
        acquire_recall_wake_lease(dir.path(), &RecallWakeLeaseOptions::default()).expect("lease");
    assert!(lease.try_release().expect("first release reports ok(true)"));
    assert!(
        !lease
            .try_release()
            .expect("second release reports ok(false)")
    );
    assert!(
        !lease.release(),
        "the retained bool API mirrors the already-gone result"
    );
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
    assert_eq!(
        error.kind(),
        expected.kind(),
        "the real io kind is preserved, not collapsed to false"
    );
    assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn bounded_wait_preserves_live_records_and_cleans_ticket() {
    let dir = tempdir().expect("directory");
    let first = acquire_recall_wake_lease(dir.path(), &Default::default()).expect("first");
    let second = acquire_recall_wake_lease(dir.path(), &Default::default()).expect("second");
    let path = recall_wake_lock_path(dir.path(), 1).expect("path");
    let before = std::fs::read(&path).expect("record");
    let result = acquire_recall_wake_lease(
        dir.path(),
        &RecallWakeLeaseOptions {
            wait_timeout_ms: Some(30),
            retry_delay_ms: Some(1),
            ..Default::default()
        },
    );
    match result {
        Err(RecallWakeError::Busy(busy)) => assert!(busy.waited_ms >= 30),
        other => panic!("expected deadline busy, got {other:?}"),
    }
    assert_eq!(std::fs::read(path).expect("unchanged"), before);
    assert!(
        list_tickets(&recall_wake_ticket_directory(dir.path()))
            .expect("tickets")
            .is_empty()
    );
    first.release();
    second.release();
}

#[test]
fn dead_slots_are_reclaimed_but_unprovable_identity_is_not() {
    let dir = tempdir().expect("directory");
    let mut dead = create_lock_record("recall-wake", Default::default()).expect("record");
    dead.pid = 99_999_999;
    for slot in 1..=2 {
        fs::write(
            &recall_wake_lock_path(dir.path(), slot).expect("path"),
            serde_json::to_string(&dead).expect("json").as_bytes(),
        )
        .expect("write");
    }
    let first = acquire_recall_wake_lease(dir.path(), &Default::default()).expect("recover first");
    let second =
        acquire_recall_wake_lease(dir.path(), &Default::default()).expect("recover second");
    assert_eq!((first.slot, second.slot), (1, 2));
    first.release();
    second.release();
    let mut owner = create_lock_record("recall-wake", Default::default()).expect("record");
    owner.process_start = "unavailable".into();
    let path = recall_wake_lock_path(dir.path(), 1).expect("path");
    fs::write(
        &path,
        serde_json::to_string(&owner).expect("json").as_bytes(),
    )
    .expect("write");
    assert!(matches!(
        acquire_recall_wake_lease(
            dir.path(),
            &RecallWakeLeaseOptions {
                max_concurrent: Some(1),
                ..Default::default()
            }
        ),
        Err(RecallWakeError::Busy(_))
    ));
}

#[test]
fn callback_unwind_releases_slot() {
    let dir = tempdir().expect("directory");
    let result = std::panic::catch_unwind(|| {
        let _ = with_recall_wake_lease(dir.path(), &Default::default(), |_| panic!("wake failed"));
    });
    assert!(result.is_err());
    let lease = acquire_recall_wake_lease(dir.path(), &Default::default()).expect("released slot");
    assert_eq!(lease.slot, 1);
    lease.release();
}

#[test]
fn ten_contenders_acquire_in_fifo_order() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::Duration;
    let dir = tempdir().expect("directory");
    let mut leases = vec![
        acquire_recall_wake_lease(dir.path(), &Default::default()).expect("first"),
        acquire_recall_wake_lease(dir.path(), &Default::default()).expect("second"),
    ];
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let mut threads = Vec::new();
    for index in 2..10 {
        let path = dir.path().to_owned();
        let acquired = acquired_tx.clone();
        let (queued_tx, queued_rx) = mpsc::channel();
        threads.push(std::thread::spawn(move || {
            let signalled = AtomicBool::new(false);
            let observe = || {
                if !signalled.load(Ordering::SeqCst)
                    && list_tickets(&recall_wake_ticket_directory(&path))
                        .expect("tickets")
                        .len()
                        == index - 1
                    && !signalled.swap(true, Ordering::SeqCst) {
                        queued_tx.send(()).expect("queued signal");
                    }
                false
            };
            let lease = acquire_recall_wake_lease(
                &path,
                &RecallWakeLeaseOptions {
                    wait_timeout_ms: Some(10_000),
                    retry_delay_ms: Some(1),
                    cancellation: Some(&observe),
                    ..Default::default()
                },
            )
            .expect("waiter");
            acquired.send((index, lease)).expect("acquired signal");
        }));
        queued_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("ticket publication event");
    }
    for next in 2..10 {
        assert!(leases[next - 2].release());
        let (index, lease) = acquired_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("next lease event");
        assert_eq!(index, next);
        leases.push(lease);
    }
    for lease in &leases[8..] {
        assert!(lease.release());
    }
    for thread in threads {
        thread.join().expect("waiter thread");
    }
    assert!(
        list_tickets(&recall_wake_ticket_directory(dir.path()))
            .expect("tickets")
            .is_empty()
    );
}

#[test]
fn reused_pid_is_reclaimed_on_conflicting_identity() {
    let directory = tempdir().expect("directory");
    let mut record = create_lock_record("recall-wake", Default::default()).expect("record");
    assert_ne!(
        record.process_start, "unavailable",
        "host must provide process identity"
    );
    record.process_start.push_str("-recycled");
    let path = recall_wake_lock_path(directory.path(), 1).expect("path");
    fs::write(
        &path,
        serde_json::to_string(&record).expect("json").as_bytes(),
    )
    .expect("write");
    let lease = acquire_recall_wake_lease(directory.path(), &Default::default()).expect("reclaim");
    assert_eq!(lease.slot, 1);
    assert!(lease.release());
}

#[test]
fn abort_after_publication_withdraws_waiting_ticket() {
    let directory = tempdir().expect("directory");
    let holder = acquire_recall_wake_lease(directory.path(), &Default::default()).expect("holder");
    let cancellation = || {
        !list_tickets(&recall_wake_ticket_directory(directory.path()))
            .expect("tickets")
            .is_empty()
    };
    let result = acquire_recall_wake_lease(
        directory.path(),
        &RecallWakeLeaseOptions {
            max_concurrent: Some(1),
            wait_timeout_ms: Some(60_000),
            cancellation: Some(&cancellation),
            ..Default::default()
        },
    );
    assert!(matches!(result, Err(RecallWakeError::Aborted)));
    assert!(
        list_tickets(&recall_wake_ticket_directory(directory.path()))
            .expect("tickets")
            .is_empty()
    );
    assert!(holder.release());
}
