//! File locks and atomic writes.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::thread;
use std::time::Duration;

pub use super::lock_lease::{LockLease, LockLeaseOptions, create_lock_lease};
use super::lock_publish::publish_lock_record;
pub use super::lock_reclaim::{
    LockReclaimDeps, LockReclaimOutcome, remove_lock_record_if_unchanged,
};
pub use super::lock_record::{
    LOCK_OWNER_INSTANCE_ID as lock_owner_instance_id, LockHolder, parse_lock_holder,
};
use super::lock_record::{
    build_lock_record, forget_lock_record, is_lock_record_stale, is_path_absence_error,
    mark_lock_record_released, read_lock_record,
};
use crate::clock::now_ms;
use crate::error::{Result, TeamCoreError};
use crate::tolerant_fsync::tolerant_fsync;

const LOCK_RETRY_MS: u64 = 50;
const LOCK_WAIT_TIMEOUT_MS: i64 = 15_000;
const DEFAULT_STALE_AFTER_MS: i64 = 300_000;

/// `withLock` options.
#[derive(Debug, Clone, Default)]
pub struct LockOptions {
    pub stale_after_ms: Option<i64>,
    pub owner_tag: Option<String>,
}

impl LockOptions {
    #[must_use]
    pub fn owner(tag: impl Into<String>) -> Self {
        Self {
            stale_after_ms: None,
            owner_tag: Some(tag.into()),
        }
    }
}

type OpenFn<'a> = Box<dyn Fn(&Path) -> io::Result<File> + 'a>;
type RenameFn<'a> = Box<dyn Fn(&Path, &Path) -> io::Result<()> + 'a>;
type PathOpFn<'a> = Box<dyn Fn(&Path) -> io::Result<()> + 'a>;

/// Injectable operations for [`atomic_write_with`]; `None` uses the real filesystem.
#[derive(Default)]
pub struct AtomicWriteDeps<'a> {
    /// Opens the temp file exclusively (the TypeScript `"wx"` flag).
    pub open: Option<OpenFn<'a>>,
    pub rename: Option<RenameFn<'a>>,
    pub rm: Option<PathOpFn<'a>>,
}

/// Injectable operations for [`assert_retryable_lock_open_error_with`].
#[derive(Default)]
pub struct LockOpenErrorDeps<'a> {
    pub access: Option<PathOpFn<'a>>,
    /// `process.platform`, e.g. `"linux"`, `"darwin"`, `"win32"`.
    pub platform: Option<&'a str>,
}

pub type LockReleaseDeps<'a> = LockReclaimDeps<'a>;

fn path_may_exist(path: &Path, deps: &LockOpenErrorDeps<'_>) -> bool {
    let result = match &deps.access {
        Some(access) => access(path),
        None => fs::symlink_metadata(path).map(|_| ()),
    };
    match result {
        Ok(()) => true,
        Err(error) => !is_path_absence_error(&error),
    }
}

/// `assertRetryableLockOpenError` with the real filesystem and platform.
pub fn assert_retryable_lock_open_error(lock_path: &Path, error: io::Error) -> io::Result<()> {
    assert_retryable_lock_open_error_with(lock_path, error, &LockOpenErrorDeps::default())
}

/// `assertRetryableLockOpenError`: `Ok` when the open failure may be contention.
pub fn assert_retryable_lock_open_error_with(
    lock_path: &Path,
    error: io::Error,
    deps: &LockOpenErrorDeps<'_>,
) -> io::Result<()> {
    if error.kind() == io::ErrorKind::AlreadyExists {
        return Ok(());
    }
    if error.raw_os_error() == Some(libc::EPERM)
        || (cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 1314)))
    {
        if path_may_exist(lock_path, deps) {
            return Ok(());
        }
        let platform = deps.platform.unwrap_or(std::env::consts::OS);
        if matches!(platform, "win32" | "windows")
            && let Some(parent) = lock_path.parent()
            && path_may_exist(parent, deps)
        {
            return Ok(());
        }
    }
    Err(error)
}

fn acquire_lock(lock_path: &Path, owner_tag: &str, stale_after_ms: i64) -> Result<String> {
    let started_at = now_ms();
    loop {
        if now_ms() - started_at > LOCK_WAIT_TIMEOUT_MS {
            let holder =
                read_lock_record(lock_path).and_then(|record| parse_lock_holder(&record.content));
            return Err(TeamCoreError::LockWaitTimeout {
                lock_path: lock_path.display().to_string(),
                holder,
            });
        }
        let record = build_lock_record(owner_tag);
        if publish_lock_record(lock_path, &record)? {
            return Ok(record);
        }
        match reap_stale_lock_with_options(lock_path, stale_after_ms, &LockReclaimDeps::default())?
        {
            LockReclaimOutcome::Removed | LockReclaimOutcome::Absent => continue,
            LockReclaimOutcome::Changed | LockReclaimOutcome::Busy | LockReclaimOutcome::Stuck => {
                thread::sleep(Duration::from_millis(LOCK_RETRY_MS))
            }
        }
    }
}

/// `withLock` receiving a [`LockLease`] for worker-handover critical sections (`#11262`).
pub fn with_lock_lease<T>(
    lock_path: &Path,
    run: impl FnOnce(&LockLease<'_>) -> Result<T>,
    opts: Option<&LockOptions>,
) -> Result<T> {
    let stale_after_ms = opts
        .and_then(|opts| opts.stale_after_ms)
        .unwrap_or(DEFAULT_STALE_AFTER_MS);
    let owner_tag = opts
        .and_then(|opts| opts.owner_tag.as_deref())
        .unwrap_or("owner");
    let record = acquire_lock(lock_path, owner_tag, stale_after_ms)?;
    let lease = create_lock_lease(
        lock_path,
        record.clone(),
        owner_tag,
        LockLeaseOptions {
            stale_after_ms: Some(stale_after_ms),
            before_restore_rename: None,
        },
    );
    let result = run(&lease);
    mark_lock_record_released(&record);
    let outcome = remove_lock_record_if_unchanged(
        lock_path,
        &record,
        stale_after_ms,
        &LockReclaimDeps::default(),
    )?;
    if matches!(
        outcome,
        LockReclaimOutcome::Removed | LockReclaimOutcome::Absent | LockReclaimOutcome::Changed
    ) {
        forget_lock_record(&record);
    }
    result
}

/// `withLock`: run `run` while holding the lock file; the lock is released afterwards.
pub fn with_lock<T>(
    lock_path: &Path,
    run: impl FnOnce() -> Result<T>,
    opts: Option<&LockOptions>,
) -> Result<T> {
    with_lock_lease(lock_path, |_| run(), opts)
}

/// `detectStaleLock`: observe and apply the record's owner and age rules.
#[must_use]
pub fn detect_stale_lock(lock_path: &Path, stale_after_ms: i64) -> bool {
    read_lock_record(lock_path).is_some_and(|record| is_lock_record_stale(&record, stale_after_ms))
}

/// `reapStaleLock` with the real filesystem.
pub fn reap_stale_lock(lock_path: &Path) -> io::Result<LockReclaimOutcome> {
    reap_stale_lock_with(lock_path, &LockReclaimDeps::default())
}

pub fn reap_stale_lock_with_threshold(
    lock_path: &Path,
    stale_after_ms: i64,
) -> io::Result<LockReclaimOutcome> {
    reap_stale_lock_with_options(lock_path, stale_after_ms, &LockReclaimDeps::default())
}

pub fn reap_stale_lock_with(
    lock_path: &Path,
    deps: &LockReclaimDeps<'_>,
) -> io::Result<LockReclaimOutcome> {
    reap_stale_lock_with_options(lock_path, DEFAULT_STALE_AFTER_MS, deps)
}

pub fn reap_stale_lock_with_options(
    lock_path: &Path,
    stale_after_ms: i64,
    deps: &LockReclaimDeps<'_>,
) -> io::Result<LockReclaimOutcome> {
    let Some(record) = read_lock_record(lock_path) else {
        return Ok(LockReclaimOutcome::Absent);
    };
    if !is_lock_record_stale(&record, stale_after_ms) {
        return Ok(LockReclaimOutcome::Changed);
    }
    remove_lock_record_if_unchanged(lock_path, &record.content, stale_after_ms, deps)
}

/// `atomicWrite` with the real filesystem.
pub fn atomic_write(file_path: &Path, content: impl AsRef<[u8]>) -> io::Result<()> {
    atomic_write_with(file_path, content, &AtomicWriteDeps::default())
}

/// `atomicWrite`: write `{path}.tmp.{uuid}` exclusively, fsync, rename; remove the temp on error.
pub fn atomic_write_with(
    file_path: &Path,
    content: impl AsRef<[u8]>,
    deps: &AtomicWriteDeps<'_>,
) -> io::Result<()> {
    let mut tmp_name = file_path.as_os_str().to_owned();
    tmp_name.push(format!(".tmp.{}", uuid::Uuid::new_v4()));
    let tmp_path = std::path::PathBuf::from(tmp_name);
    let result = (|| {
        let mut file = match &deps.open {
            Some(open) => open(&tmp_path)?,
            None => OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)?,
        };
        file.write_all(content.as_ref())?;
        tolerant_fsync(&file, "atomicWrite")?;
        drop(file);
        match &deps.rename {
            Some(rename) => rename(&tmp_path, file_path),
            None => fs::rename(&tmp_path, file_path),
        }
    })();
    if let Err(error) = result {
        let removal = match &deps.rm {
            Some(rm) => rm(&tmp_path),
            None => fs::remove_file(&tmp_path),
        };
        if let Err(removal_error) = removal
            && removal_error.kind() != io::ErrorKind::NotFound
        {
            return Err(removal_error);
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod threshold_tests {
    use super::*;
    use crate::team_state_store::lock_worker_lifetime::mark_lock_record_held;

    #[test]
    fn release_preserves_a_republished_record() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("lock");
        let replacement = build_lock_record("replacement");
        with_lock(
            &path,
            || {
                assert!(!detect_stale_lock(&path, 0));
                fs::remove_file(&path)?;
                assert!(publish_lock_record(&path, &replacement)?);
                Ok(())
            },
            None,
        )
        .expect("with lock");
        assert_eq!(
            fs::read_to_string(&path).expect("replacement survives"),
            replacement
        );
        mark_lock_record_released(&replacement);
    }

    #[test]
    fn acquisition_reclaims_own_leftover_and_recycled_pid() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("lock");
        let pid = std::process::id();
        let leftover = build_lock_record("owner");
        mark_lock_record_held(&leftover);
        mark_lock_record_released(&leftover);
        fs::write(&path, &leftover).expect("leftover");
        assert!(detect_stale_lock(&path, 300_000));
        assert_eq!(
            with_lock(&path, || Ok("acquired"), None).expect("acquire"),
            "acquired"
        );
        assert!(!path.exists());
        forget_lock_record(&leftover);

        if let Some(actual) = memory_core::locks::get_process_start_identity(pid) {
            let scheme = actual.split(':').next().unwrap_or("linux-proc-start-ticks");
            fs::write(
                &path,
                format!(
                    "owner {}\n{pid}\n{}\nforeign-instance\n{scheme}:0\n",
                    uuid::Uuid::new_v4(),
                    now_ms()
                ),
            )
            .expect("recycled leftover");
            assert!(detect_stale_lock(&path, 300_000));
            assert_eq!(
                with_lock(&path, || Ok("acquired"), None).expect("acquire"),
                "acquired"
            );
            assert!(!path.exists());
        }
    }

    #[test]
    fn torn_lock_contenders_never_overlap() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("lock");
        fs::write(&path, "").expect("torn lock");
        filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(1, 0))
            .expect("old mtime");
        let active = AtomicUsize::new(0);
        let entries = AtomicUsize::new(0);
        thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..4 {
                let (path, active, entries) = (&path, &active, &entries);
                handles.push(scope.spawn(move || {
                    for _ in 0..25 {
                        with_lock(
                            path,
                            || {
                                assert_eq!(active.fetch_add(1, Ordering::SeqCst), 0);
                                entries.fetch_add(1, Ordering::SeqCst);
                                assert_eq!(active.fetch_sub(1, Ordering::SeqCst), 1);
                                Ok(())
                            },
                            None,
                        )
                        .expect("acquire");
                    }
                }));
            }
            for handle in handles {
                handle.join().expect("join contender");
            }
        });
        assert_eq!(entries.load(Ordering::SeqCst), 100);
        assert!(!path.exists());
        assert_eq!(fs::read_dir(root.path()).expect("read dir").count(), 0);
    }

    #[test]
    fn threshold_reaper_preserves_live_owner_lock() {
        let directory =
            std::env::temp_dir().join(format!("team-core-lock-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).expect("create temporary directory");
        let path = directory.join("claim.lock");
        fs::write(
            &path,
            format!("test-owner\n{}\n{}\n", std::process::id(), now_ms()),
        )
        .expect("write lock");

        reap_stale_lock_with_threshold(&path, 300_000).expect("reap live lock");

        assert!(path.exists());
        fs::remove_dir_all(directory).expect("remove temporary directory");
    }

    #[test]
    fn threshold_reaper_removes_expired_dead_owner_lock() {
        let directory =
            std::env::temp_dir().join(format!("team-core-lock-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).expect("create temporary directory");
        let path = directory.join("claim.lock");
        fs::write(&path, "test-owner\n2147483647\n1\n").expect("write lock");

        reap_stale_lock_with_threshold(&path, 0).expect("reap dead lock");

        assert!(!path.exists());
        fs::remove_dir_all(directory).expect("remove temporary directory");
    }
}
