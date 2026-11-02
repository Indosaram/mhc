#![allow(clippy::unwrap_used)]

use std::fs::{self, OpenOptions};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Duration;
use ulw_loop_core::runtime::UlwLoopError;
use ulw_loop_core::state_lock::{
    guard_path_for, with_state_lock, with_state_lock_sync, StateLockOptions,
    ULW_LOOP_LOCK_TIMEOUT_CODE,
};

fn opts(timeout_ms: u64) -> StateLockOptions {
    StateLockOptions {
        timeout_ms: Some(timeout_ms),
        ..Default::default()
    }
}

fn lock() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".omo/ulw-loop/s1/.state.lock");
    (dir, path)
}

fn record(pid: u32, token: &str, lease_until: Option<u64>) -> String {
    serde_json::json!({
        "pid": pid,
        "createdAt": "2026-05-23T00:00:00.000Z",
        "token": token,
        "leaseUntil": lease_until
    })
    .to_string()
}

#[test]
fn expired_live_lease_is_reclaimed() {
    let (_d, path) = lock();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, record(std::process::id(), "expired", Some(0))).unwrap();
    assert_eq!(
        with_state_lock(&path, Some(&opts(0)), |_| Ok("acquired")).unwrap(),
        "acquired"
    );
}

#[test]
fn acquired_token_is_passed_to_body() {
    let (_d, path) = lock();
    with_state_lock(&path, None, |token| {
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(token));
        Ok(())
    })
    .unwrap();
}

#[test]
fn sync_holder_writes_lease_record() {
    let (_d, path) = lock();
    with_state_lock_sync(&path, None, |_| {
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(value["leaseUntil"].is_number());
        Ok(())
    })
    .unwrap();
}

#[test]
fn current_owner_times_out_without_running_body() {
    let (_d, path) = lock();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let guard_path = guard_path_for(&path);
    let guard_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&guard_path)
        .unwrap();
    guard_file.try_lock().unwrap();

    let mut ran = false;
    let err = with_state_lock(&path, Some(&opts(0)), |_| {
        ran = true;
        Ok::<(), UlwLoopError>(())
    })
    .unwrap_err();
    assert_eq!(err.code, ULW_LOOP_LOCK_TIMEOUT_CODE);
    assert!(!ran);
}

#[test]
fn timeout_error_is_ulw_loop_error() {
    let (_d, path) = lock();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let guard_path = guard_path_for(&path);
    let guard_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&guard_path)
        .unwrap();
    guard_file.try_lock().unwrap();

    let err = with_state_lock(&path, Some(&opts(0)), |_| Ok::<(), UlwLoopError>(())).unwrap_err();
    assert_eq!(err.code, ULW_LOOP_LOCK_TIMEOUT_CODE);
}

#[test]
fn dead_pid_without_lease_is_reclaimed() {
    let (_d, path) = lock();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, record(u32::MAX, "gone", None)).unwrap();
    assert!(with_state_lock(&path, Some(&opts(0)), |_| Ok::<_, UlwLoopError>(())).is_ok());
}

#[test]
fn leftover_stale_lock_record_with_dead_pid_does_not_block_acquisition() {
    let (_d, path) = lock();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, record(u32::MAX, "dead_pid", None)).unwrap();
    let result = with_state_lock(&path, Some(&opts(0)), |_| Ok::<_, UlwLoopError>("acquired"));
    assert_eq!(result.unwrap(), "acquired");
}

#[test]
fn two_threads_contend_for_guard_never_exceeds_one_holder() {
    let (_d, path) = lock();
    let holder_count = Arc::new(AtomicUsize::new(0));
    let max_holders = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(2));

    let mut handles = Vec::new();
    for _ in 0..2 {
        let p = path.clone();
        let hc = holder_count.clone();
        let mh = max_holders.clone();
        let b = barrier.clone();
        handles.push(std::thread::spawn(move || {
            b.wait();
            with_state_lock(&p, Some(&opts(3_000)), |_| {
                let current = hc.fetch_add(1, Ordering::SeqCst) + 1;
                mh.fetch_max(current, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(50));
                hc.fetch_sub(1, Ordering::SeqCst);
                Ok::<(), UlwLoopError>(())
            })
        }));
    }

    for h in handles {
        h.join().unwrap().unwrap();
    }

    assert_eq!(max_holders.load(Ordering::SeqCst), 1);
}

#[test]
fn stale_lock_reclaim_admits_one_holder() {
    let (_d, path) = lock();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, record(u32::MAX, "stale", None)).unwrap();

    let barrier_holder = Arc::new(Barrier::new(2));
    let barrier_wait = barrier_holder.clone();

    let p1 = path.clone();
    let handle_a = std::thread::spawn(move || {
        with_state_lock(&p1, Some(&opts(2_000)), |_| {
            barrier_holder.wait();
            std::thread::sleep(Duration::from_millis(150));
            Ok::<_, UlwLoopError>("holder_a")
        })
    });

    barrier_wait.wait();

    let p2 = path.clone();
    let handle_b = std::thread::spawn(move || {
        with_state_lock(&p2, Some(&opts(50)), |_| {
            Ok::<_, UlwLoopError>("holder_b")
        })
    });

    let res_b = handle_b.join().unwrap();
    let res_a = handle_a.join().unwrap();

    assert_eq!(res_a.unwrap(), "holder_a");
    let err_b = res_b.unwrap_err();
    assert_eq!(err_b.code, ULW_LOOP_LOCK_TIMEOUT_CODE);
}

#[test]
fn prior_failure_interleaving_a_acquires_b_times_out() {
    stale_lock_reclaim_admits_one_holder();
}

#[test]
fn body_error_propagates() {
    let (_d, path) = lock();
    let err = with_state_lock(&path, None, |_| {
        Err::<(), _>(UlwLoopError::new("body exploded", "BODY"))
    })
    .unwrap_err();
    assert_eq!(err.code, "BODY");
    assert!(!path.exists());
}

#[test]
fn successor_lock_is_preserved() {
    let (_d, path) = lock();
    with_state_lock(&path, None, |_| {
        fs::write(&path, record(std::process::id(), "successor", None)).unwrap();
        Ok::<_, UlwLoopError>(())
    })
    .unwrap();
    assert!(path.exists());
    assert!(fs::read_to_string(path).unwrap().contains("successor"));
}

#[test]
fn sync_acquisition_releases_lock() {
    let (_d, path) = lock();
    with_state_lock_sync(&path, None, |_| Ok::<_, UlwLoopError>(())).unwrap();
    assert!(!path.exists());
}

#[test]
fn sync_current_owner_times_out() {
    let (_d, path) = lock();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let guard_path = guard_path_for(&path);
    let guard_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&guard_path)
        .unwrap();
    guard_file.try_lock().unwrap();

    let e = with_state_lock_sync(&path, Some(&opts(0)), |_| Ok::<_, UlwLoopError>(())).unwrap_err();
    assert_eq!(e.code, ULW_LOOP_LOCK_TIMEOUT_CODE);
}

#[test]
fn nested_replacement_is_not_removed() {
    let (_d, path) = lock();
    with_state_lock(&path, None, |_| {
        fs::write(&path, record(std::process::id(), "replacement", None)).unwrap();
        Ok::<_, UlwLoopError>(())
    })
    .unwrap();
    assert!(path.exists());
}

#[test]
fn lock_parent_directories_are_created() {
    let (_d, path) = lock();
    with_state_lock(&path, None, |_| Ok::<_, UlwLoopError>(())).unwrap();
    assert!(!path.exists());
}
