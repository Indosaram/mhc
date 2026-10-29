use std::fs;

use pretty_assertions::assert_eq;

use super::*;
use crate::locks::lock_record::{CreateLockRecordOptions, create_lock_record};

#[test]
fn given_absent_lock_when_acquired_and_released_then_record_published_and_removed() {
    // #given
    let temp_dir = tempfile::tempdir().unwrap();
    let lock_path = temp_dir.path().join("resource.lock");
    let record = create_lock_record(
        "memory-write",
        CreateLockRecordOptions {
            run_id: Some("run-1".to_string()),
        },
    )
    .unwrap();

    // #when
    acquire_lock(&lock_path, &record, &AcquireLockOptions::default()).unwrap();
    let published_raw = fs::read_to_string(&lock_path).unwrap();
    let published: LockRecord = serde_json::from_str(published_raw.trim()).unwrap();

    // #then
    assert_eq!(published, record);
    assert!(is_held(&lock_path));
    assert!(release_lock(&lock_path, &record).unwrap());
    assert!(!is_held(&lock_path));
}

#[test]
fn given_owned_lock_when_another_owner_acquires_then_contention_error_raised() {
    // #given
    let temp_dir = tempfile::tempdir().unwrap();
    let lock_path = temp_dir.path().join("resource.lock");
    let owner = create_lock_record("memory-write", CreateLockRecordOptions::default()).unwrap();
    acquire_lock(&lock_path, &owner, &AcquireLockOptions::default()).unwrap();

    // #when
    let contender = create_lock_record("memory-write", CreateLockRecordOptions::default()).unwrap();
    let error = acquire_lock(&lock_path, &contender, &AcquireLockOptions::default()).unwrap_err();

    // #then
    match error {
        AcquireLockError::Contention(contention) => {
            assert!(contention.retriable());
            assert_eq!(contention.lock_path, lock_path);
            assert_eq!(contention.owner, Some(owner.clone()));
        }
        other => panic!("expected contention error, got {other:?}"),
    }

    assert!(release_lock(&lock_path, &owner).unwrap());
}

#[test]
fn given_lock_whose_nonce_changed_when_former_owner_releases_then_replacement_remains_held() {
    // #given
    let temp_dir = tempfile::tempdir().unwrap();
    let lock_path = temp_dir.path().join("resource.lock");
    let former_owner =
        create_lock_record("memory-write", CreateLockRecordOptions::default()).unwrap();
    let replacement =
        create_lock_record("memory-write", CreateLockRecordOptions::default()).unwrap();
    fs::write(
        &lock_path,
        format!("{}\n", serde_json::to_string(&replacement).unwrap()),
    )
    .unwrap();

    // #when
    let released = release_lock(&lock_path, &former_owner).unwrap();

    // #then
    assert!(!released);
    let published_raw = fs::read_to_string(&lock_path).unwrap();
    let published: LockRecord = serde_json::from_str(published_raw.trim()).unwrap();
    assert_eq!(published, replacement);
}

#[test]
fn given_live_pid_with_different_process_start_when_acquisition_attempted_then_recovered() {
    // #given
    let temp_dir = tempfile::tempdir().unwrap();
    let lock_path = temp_dir.path().join("resource.lock");
    let mut reused_pid_owner =
        create_lock_record("memory-write", CreateLockRecordOptions::default()).unwrap();
    let actual_start = get_process_start_identity(reused_pid_owner.pid);
    assert!(actual_start.is_some());
    let actual_scheme = actual_start
        .as_deref()
        .and_then(|s| s.split_once(':').map(|(scheme, _)| scheme))
        .unwrap_or("legacy");
    reused_pid_owner.process_start = format!("{actual_scheme}:different-process-start");
    fs::write(
        &lock_path,
        format!("{}\n", serde_json::to_string(&reused_pid_owner).unwrap()),
    )
    .unwrap();

    // #when
    let successor = create_lock_record("memory-write", CreateLockRecordOptions::default()).unwrap();
    acquire_lock(&lock_path, &successor, &AcquireLockOptions::default()).unwrap();

    // #then
    let published_raw = fs::read_to_string(&lock_path).unwrap();
    let published: LockRecord = serde_json::from_str(published_raw.trim()).unwrap();
    assert_eq!(published, successor);
    assert!(release_lock(&lock_path, &successor).unwrap());
}

#[test]
fn given_old_lock_whose_owner_is_live_when_acquisition_attempted_then_never_recovered() {
    // #given
    let temp_dir = tempfile::tempdir().unwrap();
    let lock_path = temp_dir.path().join("resource.lock");
    let mut live_owner =
        create_lock_record("memory-write", CreateLockRecordOptions::default()).unwrap();
    live_owner.created_at = "2000-01-01T00:00:00.000Z".to_string();
    fs::write(
        &lock_path,
        format!("{}\n", serde_json::to_string(&live_owner).unwrap()),
    )
    .unwrap();

    // #when
    let contender = create_lock_record("memory-write", CreateLockRecordOptions::default()).unwrap();
    let error = acquire_lock(&lock_path, &contender, &AcquireLockOptions::default()).unwrap_err();

    // #then
    match error {
        AcquireLockError::Contention(c) => assert_eq!(c.owner, Some(live_owner)),
        other => panic!("expected contention error, got {other:?}"),
    }
}

#[test]
fn given_dead_owner_on_another_host_when_acquisition_attempted_then_recovery_fails_closed() {
    // #given
    let temp_dir = tempfile::tempdir().unwrap();
    let lock_path = temp_dir.path().join("resource.lock");
    let mut owner =
        create_lock_record("reflection-scheduler", CreateLockRecordOptions::default()).unwrap();
    owner.pid = 2_000_000_000;
    owner.hostname = "foreign-host.invalid".to_string();
    fs::write(
        &lock_path,
        format!("{}\n", serde_json::to_string(&owner).unwrap()),
    )
    .unwrap();

    // #when
    let contender =
        create_lock_record("reflection-scheduler", CreateLockRecordOptions::default()).unwrap();
    let error = acquire_lock(&lock_path, &contender, &AcquireLockOptions::default()).unwrap_err();

    // #then
    match error {
        AcquireLockError::Contention(c) => assert_eq!(c.owner, Some(owner)),
        other => panic!("expected contention error, got {other:?}"),
    }
}

#[test]
fn given_callback_under_lock_when_callback_fails_then_with_lock_releases_lock() {
    // #given
    let temp_dir = tempfile::tempdir().unwrap();
    let lock_path = temp_dir.path().join("resource.lock");
    let record =
        create_lock_record("transcript-state", CreateLockRecordOptions::default()).unwrap();

    // #when
    let error = with_lock(
        &lock_path,
        &record,
        &AcquireLockOptions::default(),
        || -> Result<(), &'static str> { Err("callback failed") },
    );

    // #then
    assert!(error.is_err());
    let err_string = error.unwrap_err().to_string();
    assert!(err_string.contains("callback failed"));
    assert!(!is_held(&lock_path));
}

#[test]
fn given_cancelled_option_when_acquire_called_then_returns_aborted() {
    // #given
    let temp_dir = tempfile::tempdir().unwrap();
    let lock_path = temp_dir.path().join("resource.lock");
    let record = create_lock_record("notice", CreateLockRecordOptions::default()).unwrap();
    let is_cancelled = || true;
    let options = AcquireLockOptions {
        wait_timeout_ms: Some(100),
        retry_delay_ms: Some(25),
        cancellation: Some(&is_cancelled),
    };

    // #when
    let result = acquire_lock(&lock_path, &record, &options);

    // #then
    assert!(matches!(result, Err(AcquireLockError::Aborted)));
}

fn write_aged_lock(path: &std::path::Path, raw: &str) {
    fs::write(path, raw).unwrap();
    filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(100, 0)).unwrap();
}

fn recovery_now(age_ms: u64) -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_millis(100_000 + age_ms)
}

#[test]
fn incomplete_primary_is_reclaimed_only_after_grace() {
    // #given
    for age_ms in [0, 4_999, 5_000, 5_001] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("resource.lock");
        write_aged_lock(&path, "{\"pid\":");
        let contender =
            create_lock_record("memory-write", CreateLockRecordOptions::default()).unwrap();

        // #when
        let result =
            acquire_lock_with_clock(&path, &contender, &AcquireLockOptions::default(), || {
                recovery_now(age_ms)
            });

        // #then
        if age_ms > 5_000 {
            result.unwrap();
            assert_eq!(read_owner(&path).unwrap().unwrap().record, Some(contender));
        } else {
            assert!(matches!(result, Err(AcquireLockError::Contention(_))));
            assert_eq!(fs::read_to_string(&path).unwrap(), "{\"pid\":");
        }
    }
}

#[test]
fn doubly_incomplete_locks_are_recovered_in_one_nonwaiting_acquisition() {
    // #given
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("resource.lock");
    let recovery = temp.path().join("resource.lock.recovery");
    write_aged_lock(&path, "");
    write_aged_lock(&recovery, "{");
    let contender = create_lock_record("memory-write", CreateLockRecordOptions::default()).unwrap();

    // #when
    acquire_lock_with_clock(&path, &contender, &AcquireLockOptions::default(), || {
        recovery_now(5_001)
    })
    .unwrap();

    // #then
    assert_eq!(read_owner(&path).unwrap().unwrap().record, Some(contender));
    assert!(!recovery.exists());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[test]
fn incomplete_recovery_lock_is_protected_through_grace() {
    // #given
    for age_ms in [0, 4_999, 5_000] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("resource.lock");
        let recovery = temp.path().join("resource.lock.recovery");
        write_aged_lock(&path, "{");
        write_aged_lock(&recovery, "");
        let contender =
            create_lock_record("memory-write", CreateLockRecordOptions::default()).unwrap();
        let snapshot = read_owner(&path).unwrap().unwrap();
        let now =
            snapshot.metadata.as_ref().unwrap().modified().unwrap() + Duration::from_millis(5_001);
        filetime::set_file_mtime(
            &recovery,
            filetime::FileTime::from_system_time(now - Duration::from_millis(age_ms)),
        )
        .unwrap();

        // #when
        let recovered = recover_dead_owner(&path, &snapshot, &contender, now).unwrap();

        // #then
        assert!(!recovered);
        assert_eq!(fs::read_to_string(&path).unwrap(), "{");
        assert_eq!(fs::read_to_string(&recovery).unwrap(), "");
    }
}

#[test]
fn proven_dead_recovery_holder_is_reclaimed_without_age_requirement() {
    // #given
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("resource.lock");
    let recovery = temp.path().join("resource.lock.recovery");
    write_aged_lock(&path, "{");
    let contender = create_lock_record("memory-write", CreateLockRecordOptions::default()).unwrap();
    let mut dead = contender.clone();
    dead.pid = 2_000_000_000;
    fs::write(&recovery, serde_json::to_string(&dead).unwrap()).unwrap();

    // #when
    acquire_lock_with_clock(&path, &contender, &AcquireLockOptions::default(), || {
        recovery_now(5_001)
    })
    .unwrap();

    // #then
    assert_eq!(read_owner(&path).unwrap().unwrap().record, Some(contender));
    assert!(!recovery.exists());
}

#[test]
fn valid_live_foreign_and_unavailable_identity_records_are_not_reclaimed_by_age() {
    // #given
    for recovery_holder in [false, true] {
        for kind in ["live", "foreign", "unavailable"] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("resource.lock");
            let recovery = temp.path().join("resource.lock.recovery");
            let contender =
                create_lock_record("memory-write", CreateLockRecordOptions::default()).unwrap();
            let mut owner = contender.clone();
            match kind {
                "foreign" => {
                    owner.hostname = "foreign.invalid".into();
                    owner.pid = 2_000_000_000;
                }
                "unavailable" => owner.process_start = "unavailable".into(),
                _ => (),
            }
            let raw = serde_json::to_string(&owner).unwrap();
            if recovery_holder {
                write_aged_lock(&path, "{");
                write_aged_lock(&recovery, &raw);
            } else {
                write_aged_lock(&path, &raw);
            }

            // #when
            let result =
                acquire_lock_with_clock(&path, &contender, &AcquireLockOptions::default(), || {
                    recovery_now(1_000_000)
                });

            // #then
            assert!(matches!(result, Err(AcquireLockError::Contention(_))));
            let held = if recovery_holder { &recovery } else { &path };
            assert_eq!(fs::read_to_string(held).unwrap(), raw);
        }
    }
}

#[test]
#[cfg(unix)]
fn replaced_primary_snapshot_is_not_reclaimed_even_with_identical_bytes_and_mtime() {
    // #given
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("resource.lock");
    write_aged_lock(&path, "{");
    let snapshot = read_owner(&path).unwrap().unwrap();
    fs::rename(&path, temp.path().join("original")).unwrap();
    write_aged_lock(&path, "{");
    let contender = create_lock_record("memory-write", CreateLockRecordOptions::default()).unwrap();

    // #when
    let recovered = recover_dead_owner(&path, &snapshot, &contender, recovery_now(5_001)).unwrap();

    // #then
    assert!(!recovered);
    assert_eq!(fs::read_to_string(&path).unwrap(), "{");
}

#[test]
fn recovery_reaper_hands_back_replacement_without_overwriting_another_publisher() {
    // #given
    for republished in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let recovery = temp.path().join("resource.lock.recovery");
        let tombstone = temp.path().join("resource.lock.recovery.reaping-test");
        write_aged_lock(&recovery, "{");
        let stale = read_owner(&recovery).unwrap().unwrap();
        let replacement =
            create_lock_record("memory-write:recovery", CreateLockRecordOptions::default())
                .unwrap();
        let raw = serde_json::to_string(&replacement).unwrap();
        fs::write(&tombstone, &raw).unwrap();
        fs::remove_file(&recovery).unwrap();
        if republished {
            fs::write(&recovery, "third-publisher").unwrap();
        }

        // #when
        let reclaimed = finish_recovery_reap(&recovery, &tombstone, &stale).unwrap();

        // #then
        assert!(!reclaimed);
        assert_eq!(
            fs::read_to_string(&recovery).unwrap(),
            if republished { "third-publisher" } else { &raw }
        );
        assert!(!tombstone.exists());
    }
}
