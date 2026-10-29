use super::*;
use pretty_assertions::assert_eq;

/// A pid far above any real pid table (Linux caps at 2^22, macOS at 99999).
const DEAD_PID: u32 = 2_147_483_000;

#[test]
fn current_process_is_alive() {
    assert!(is_process_alive(i64::from(std::process::id())));
}

#[test]
fn dead_and_invalid_pids_are_not_alive() {
    assert!(!is_process_alive(i64::from(DEAD_PID)));
    assert!(!is_process_alive(0));
    assert!(!is_process_alive(-5));
}

#[test]
fn free_path_writes_owner_pid_and_holds_lock() {
    let root = tempfile::tempdir().expect("tempdir");
    let lock = root.path().join("nested").join("daemon.lock");
    let handle = try_acquire_lock(&lock, std::process::id())
        .expect("io")
        .expect("acquired");
    assert_eq!(read_lock_pid(&lock), Some(i64::from(std::process::id())));
    assert!(try_acquire_lock(&lock, 1).expect("io").is_none());
    handle.release();
    assert!(!lock.exists());
}

#[test]
fn stale_lock_owned_by_dead_pid_is_reaped_and_acquired() {
    let root = tempfile::tempdir().expect("tempdir");
    let lock = root.path().join("daemon.lock");
    std::fs::write(&lock, format!("{DEAD_PID}\n")).expect("stale");
    let handle = try_acquire_lock(&lock, std::process::id()).expect("io");
    assert!(handle.is_some());
    assert_eq!(read_lock_pid(&lock), Some(i64::from(std::process::id())));
}

#[test]
fn garbage_lock_contents_are_reclaimed() {
    let root = tempfile::tempdir().expect("tempdir");
    let lock = root.path().join("daemon.lock");
    std::fs::write(&lock, "not-a-pid").expect("garbage");
    assert!(try_acquire_lock(&lock, 42).expect("io").is_some());
    assert_eq!(read_lock_pid(&lock), Some(42));
}

#[test]
fn parse_leading_int_matches_number_parse_int() {
    assert_eq!(parse_leading_int("123abc"), Some(123));
    assert_eq!(parse_leading_int("-7"), Some(-7));
    assert_eq!(parse_leading_int("abc"), None);
}
