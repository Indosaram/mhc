#![allow(clippy::unwrap_used)]

use std::fs;
use std::time::{Duration, SystemTime};
use ulw_loop_core::runtime::UlwLoopError;
use ulw_loop_core::state_lock::{with_state_lock, with_state_lock_sync, StateLockOptions, ULW_LOOP_LOCK_TIMEOUT_CODE};

fn opts(timeout_ms: u64) -> StateLockOptions { StateLockOptions { timeout_ms: Some(timeout_ms), ..Default::default() } }
fn lock() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".omo/ulw-loop/s1/.state.lock");
    (dir, path)
}
fn record(pid: u32, token: &str, lease_until: Option<u64>) -> String {
    serde_json::json!({"pid":pid,"createdAt":"2026-05-23T00:00:00.000Z","token":token,"leaseUntil":lease_until}).to_string()
}

#[test]
fn expired_live_lease_is_reclaimed() {
    let (_d, path) = lock(); fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, record(std::process::id(), "expired", Some(0))).unwrap();
    assert_eq!(with_state_lock(&path, Some(&opts(0)), |_| Ok("acquired")).unwrap(), "acquired");
}
#[test]
fn acquired_token_is_passed_to_body() {
    let (_d, path) = lock();
    with_state_lock(&path, None, |token| { let text=fs::read_to_string(&path).unwrap(); assert!(text.contains(token)); Ok(()) }).unwrap();
}
#[test]
fn sync_holder_writes_lease_record() {
    let (_d, path) = lock();
    with_state_lock_sync(&path, None, |_| { let value: serde_json::Value=serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap(); assert!(value["leaseUntil"].is_number()); Ok(()) }).unwrap();
}
#[test]
fn current_owner_times_out_without_running_body() {
    let (_d, path) = lock(); fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, record(std::process::id(), "holder", Some(u64::MAX))).unwrap();
    let mut ran=false; let err=with_state_lock(&path, Some(&opts(0)), |_| {ran=true; Ok::<(), UlwLoopError>(())}).unwrap_err();
    assert_eq!(err.code, ULW_LOOP_LOCK_TIMEOUT_CODE); assert!(!ran);
}
#[test]
fn timeout_error_is_ulw_loop_error() {
    let (_d, path) = lock(); fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(&path, "not json").unwrap();
    let err=with_state_lock(&path, Some(&opts(0)), |_| Ok::<(), UlwLoopError>(())).unwrap_err(); assert_eq!(err.code, ULW_LOOP_LOCK_TIMEOUT_CODE);
}
#[test]
fn dead_pid_without_lease_is_reclaimed() {
    let (_d, path)=lock(); fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(&path, record(u32::MAX, "gone", None)).unwrap();
    assert!(with_state_lock(&path, Some(&opts(0)), |_| Ok::<_, UlwLoopError>(())).is_ok());
}
#[test]
fn young_legacy_live_record_is_not_stolen() {
    let (_d,path)=lock(); fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(&path, serde_json::json!({"pid":std::process::id(),"createdAt":"2026-05-23T00:00:00.000Z"}).to_string()).unwrap();
    let err=with_state_lock(&path, Some(&opts(0)), |_| Ok::<_, UlwLoopError>(())).unwrap_err(); assert_eq!(err.code, ULW_LOOP_LOCK_TIMEOUT_CODE);
}
#[test]
fn malformed_old_record_is_reclaimed() {
    let (_d,path)=lock(); fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(&path, "not json").unwrap();
    let old = filetime::FileTime::from_system_time(SystemTime::now() - Duration::from_secs(120));
    filetime::set_file_times(&path, old, old).unwrap();
    assert!(with_state_lock(&path, Some(&StateLockOptions { timeout_ms: Some(1_000), stale_ms: Some(60_000), ..opts(1_000) }), |_| Ok::<_, UlwLoopError>(())).is_ok());
}
#[test]
fn body_error_propagates() {
    let (_d,path)=lock(); let err=with_state_lock(&path,None, |_| Err::<(), _>(UlwLoopError::new("body exploded","BODY"))).unwrap_err(); assert_eq!(err.code,"BODY"); assert!(!path.exists());
}
#[test]
fn successor_lock_is_preserved() {
    let (_d,path)=lock(); with_state_lock(&path,None, |_| { fs::write(&path, record(std::process::id(),"successor",None)).unwrap(); Ok::<_,UlwLoopError>(()) }).unwrap();
    assert!(path.exists()); assert!(fs::read_to_string(path).unwrap().contains("successor"));
}
#[test]
fn stale_time_metadata_does_not_reclaim_live_lease() {
    let (_d,path)=lock(); fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(&path, record(std::process::id(),"live",Some(u64::MAX))).unwrap();
    let err=with_state_lock(&path,Some(&opts(0)), |_| Ok::<_,UlwLoopError>(())).unwrap_err(); assert_eq!(err.code,ULW_LOOP_LOCK_TIMEOUT_CODE);
}
#[test]
fn sync_acquisition_releases_lock() { let (_d,path)=lock(); with_state_lock_sync(&path,None, |_| Ok::<_,UlwLoopError>(())).unwrap(); assert!(!path.exists()); }
#[test]
fn sync_current_owner_times_out() { let (_d,path)=lock(); fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(&path,record(std::process::id(),"holder",Some(u64::MAX))).unwrap(); let e=with_state_lock_sync(&path,Some(&opts(0)), |_| Ok::<_,UlwLoopError>(())).unwrap_err(); assert_eq!(e.code,ULW_LOOP_LOCK_TIMEOUT_CODE); }
#[test]
fn nested_replacement_is_not_removed() { let (_d,path)=lock(); with_state_lock(&path,None, |_| { fs::write(&path,record(std::process::id(),"replacement",None)).unwrap(); Ok::<_,UlwLoopError>(()) }).unwrap(); assert!(path.exists()); }
#[test]
fn lock_parent_directories_are_created() { let (_d,path)=lock(); with_state_lock(&path,None, |_| Ok::<_,UlwLoopError>(())).unwrap(); assert!(!path.exists()); }

