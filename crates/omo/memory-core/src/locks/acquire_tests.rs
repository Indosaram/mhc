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
    reused_pid_owner.process_start = "different-process-start".to_string();
    fs::write(
        &lock_path,
        format!("{}\n", serde_json::to_string(&reused_pid_owner).unwrap()),
    )
    .unwrap();

    // #when
    let successor = create_lock_record("memory-write", CreateLockRecordOptions::default()).unwrap();
    if cfg!(target_os = "windows") {
        let error =
            acquire_lock(&lock_path, &successor, &AcquireLockOptions::default()).unwrap_err();
        match error {
            AcquireLockError::Contention(c) => assert_eq!(c.owner, Some(reused_pid_owner)),
            other => panic!("expected contention error on windows, got {other:?}"),
        }
        return;
    }
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
