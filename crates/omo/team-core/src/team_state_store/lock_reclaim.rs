use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::lock_publish::publish_lock_record;
use super::lock_record::{build_lock_record, is_lock_record_stale, is_path_absence_error, mark_lock_record_released, read_lock_record};

type DelayFn<'a> = Box<dyn Fn(u64) + 'a>;
type UnlinkFn<'a> = Box<dyn Fn(&Path) -> io::Result<()> + 'a>;

#[derive(Default)]
pub struct LockReclaimDeps<'a> {
    pub delay: Option<DelayFn<'a>>,
    pub unlink: Option<UnlinkFn<'a>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockReclaimOutcome {
    Removed,
    Absent,
    Changed,
    Busy,
    Stuck,
}

const RECLAIM_UNLINK_ATTEMPTS: u32 = 3;
const RECLAIM_UNLINK_RETRY_MS: u64 = 25;

pub fn reclaim_guard_path(lock_path: &Path, record: &str) -> PathBuf {
    let hash = format!("{:x}", Sha256::digest(record.as_bytes()));
    let mut path = lock_path.as_os_str().to_owned();
    path.push(format!(".reclaim-{}", &hash[..16]));
    PathBuf::from(path)
}

fn unlink_with_retry(path: &Path, deps: &LockReclaimDeps<'_>) -> LockReclaimOutcome {
    for attempt in 1..=RECLAIM_UNLINK_ATTEMPTS {
        let result = match &deps.unlink {
            Some(unlink) => unlink(path),
            None => fs::remove_file(path),
        };
        let Err(error) = result else { return LockReclaimOutcome::Removed; };
        if is_path_absence_error(&error) {
            return LockReclaimOutcome::Absent;
        }
        let retryable = if cfg!(windows) {
            matches!(error.raw_os_error(), Some(5 | 32 | 33 | 170))
                || matches!(error.raw_os_error(), Some(libc::EPERM | libc::EBUSY))
        } else {
            matches!(error.raw_os_error(), Some(libc::EPERM | libc::EBUSY))
        };
        if !retryable || attempt == RECLAIM_UNLINK_ATTEMPTS {
            return LockReclaimOutcome::Stuck;
        }
        match &deps.delay {
            Some(delay) => delay(RECLAIM_UNLINK_RETRY_MS),
            None => thread::sleep(Duration::from_millis(RECLAIM_UNLINK_RETRY_MS)),
        }
    }
    LockReclaimOutcome::Stuck
}

pub fn remove_lock_record_if_unchanged(
    lock_path: &Path,
    expected_record: &str,
    stale_after_ms: i64,
    deps: &LockReclaimDeps<'_>,
) -> io::Result<LockReclaimOutcome> {
    let guard_path = reclaim_guard_path(lock_path, expected_record);
    let guard_record = build_lock_record("lock-reclaim");
    if !publish_lock_record(&guard_path, &guard_record)? {
        if let Some(guard) = read_lock_record(&guard_path)
            && is_lock_record_stale(&guard, stale_after_ms)
        {
            remove_lock_record_if_unchanged(&guard_path, &guard.content, stale_after_ms, deps)?;
        }
        return Ok(LockReclaimOutcome::Busy);
    }
    let outcome = match read_lock_record(lock_path) {
        None => LockReclaimOutcome::Absent,
        Some(current) if current.content != expected_record || !is_lock_record_stale(&current, stale_after_ms) => LockReclaimOutcome::Changed,
        Some(_) => unlink_with_retry(lock_path, deps),
    };
    mark_lock_record_released(&guard_record);
    unlink_with_retry(&guard_path, deps);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::sync::{Arc, Barrier};

    fn torn_lock(path: &Path) {
        fs::write(path, "").expect("write torn record");
        let old = filetime::FileTime::from_unix_time(1, 0);
        filetime::set_file_mtime(path, old).expect("set old mtime");
    }

    #[test]
    fn concurrent_reclaimers_remove_exactly_once() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("lock");
        torn_lock(&path);
        let barrier = Arc::new(Barrier::new(8));
        let outcomes = thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..8 {
                let barrier = Arc::clone(&barrier);
                let path = &path;
                handles.push(scope.spawn(move || {
                    barrier.wait();
                    remove_lock_record_if_unchanged(path, "", 300_000, &LockReclaimDeps::default()).expect("reclaim")
                }));
            }
            handles.into_iter().map(|handle| handle.join().expect("join")).collect::<Vec<_>>()
        });
        assert_eq!(outcomes.iter().filter(|outcome| **outcome == LockReclaimOutcome::Removed).count(), 1);
        assert_eq!(fs::read_dir(root.path()).expect("read dir").count(), 0);
    }

    #[test]
    fn guard_blocks_racer_before_unlink_and_late_observer_preserves_replacement() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("lock");
        torn_lock(&path);
        let raced = Cell::new(false);
        let deps = LockReclaimDeps {
            unlink: Some(Box::new(|target| {
                if target == path {
                    assert_eq!(remove_lock_record_if_unchanged(&path, "", 300_000, &LockReclaimDeps::default()).expect("race"), LockReclaimOutcome::Busy);
                    assert!(fs::OpenOptions::new().write(true).create_new(true).open(&path).is_err());
                    raced.set(true);
                }
                fs::remove_file(target)
            })),
            delay: Some(Box::new(|_| panic!("no retries expected"))),
        };
        assert_eq!(remove_lock_record_if_unchanged(&path, "", 300_000, &deps).expect("reclaim"), LockReclaimOutcome::Removed);
        assert!(raced.get());
        let replacement = build_lock_record("replacement");
        assert!(publish_lock_record(&path, &replacement).expect("publish"));
        assert_eq!(remove_lock_record_if_unchanged(&path, "", 300_000, &LockReclaimDeps::default()).expect("late observer"), LockReclaimOutcome::Changed);
        assert_eq!(fs::read_to_string(&path).expect("read"), replacement);
        mark_lock_record_released(&replacement);
    }

    #[test]
    fn retries_transient_unlink_and_reports_stuck_or_absent() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("lock");
        torn_lock(&path);
        let calls = Cell::new(0);
        let delays = RefCell::new(Vec::new());
        let deps = LockReclaimDeps {
            delay: Some(Box::new(|ms| delays.borrow_mut().push(ms))),
            unlink: Some(Box::new(|target| {
                if target == path {
                    calls.set(calls.get() + 1);
                    if calls.get() < 3 { return Err(io::Error::from_raw_os_error(libc::EPERM)); }
                }
                fs::remove_file(target)
            })),
        };
        assert_eq!(remove_lock_record_if_unchanged(&path, "", 300_000, &deps).expect("reclaim"), LockReclaimOutcome::Removed);
        assert_eq!(calls.get(), 3);
        assert_eq!(*delays.borrow(), [25, 25]);
        assert_eq!(remove_lock_record_if_unchanged(&path, "", 300_000, &LockReclaimDeps::default()).expect("absent"), LockReclaimOutcome::Absent);
        torn_lock(&path);
        let stuck = LockReclaimDeps {
            unlink: Some(Box::new(|target| {
                if target == path { Err(io::Error::other("unlink failed")) } else { fs::remove_file(target) }
            })),
            delay: Some(Box::new(|_| panic!("nonretryable"))),
        };
        assert_eq!(remove_lock_record_if_unchanged(&path, "", 300_000, &stuck).expect("stuck"), LockReclaimOutcome::Stuck);
        assert!(path.exists());
    }

    #[test]
    fn stale_guard_is_reclaimed_but_current_call_stays_busy() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("lock");
        torn_lock(&path);
        let guard = reclaim_guard_path(&path, "");
        torn_lock(&guard);
        assert_eq!(remove_lock_record_if_unchanged(&path, "", 300_000, &LockReclaimDeps::default()).expect("busy"), LockReclaimOutcome::Busy);
        assert!(!guard.exists());
        assert!(path.exists());
        assert_eq!(reclaim_guard_path(Path::new("lock"), "").to_string_lossy(), "lock.reclaim-e3b0c44298fc1c14");
    }
}
