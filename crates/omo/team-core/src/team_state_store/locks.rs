//! File locks (owner file = `tag\npid\nms\n`) and atomic writes.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::thread;
use std::time::Duration;

use crate::clock::now_ms;
use crate::error::{Result, TeamCoreError};
use crate::tolerant_fsync::tolerant_fsync;

const LOCK_RETRY_MS: u64 = 50;
const LOCK_WAIT_TIMEOUT_MS: i64 = 15_000;
const LOCK_RELEASE_RETRY_ATTEMPTS: u32 = 3;
const LOCK_RELEASE_RETRY_MS: u64 = 25;
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
type DelayFn<'a> = Box<dyn Fn(u64) + 'a>;

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

/// Injectable operations for [`reap_stale_lock_with`].
#[derive(Default)]
pub struct LockReleaseDeps<'a> {
    pub delay: Option<DelayFn<'a>>,
    pub unlink: Option<PathOpFn<'a>>,
}

fn build_owner_content(owner_tag: &str) -> String {
    format!("{owner_tag}\n{}\n{}\n", std::process::id(), now_ms())
}

fn parse_owner_content(content: &str) -> Option<(i64, i64)> {
    let lines: Vec<&str> = content
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.is_empty())
        .collect();
    if lines.len() != 3 {
        return None;
    }
    let owner_pid = parse_int_prefix(lines[1])?;
    let acquired_at = parse_int_prefix(lines[2])?;
    (owner_pid > 0 && acquired_at > 0).then_some((owner_pid, acquired_at))
}

/// `Number.parseInt(text, 10)`: leading whitespace, optional sign, leading digits.
fn parse_int_prefix(text: &str) -> Option<i64> {
    let trimmed = text.trim_start();
    let (sign, digits) = match trimmed.as_bytes().first() {
        Some(b'-') => (-1, &trimmed[1..]),
        Some(b'+') => (1, &trimmed[1..]),
        _ => (1, trimmed),
    };
    let end = digits.bytes().take_while(u8::is_ascii_digit).count();
    digits[..end].parse::<i64>().ok().map(|value| sign * value)
}

fn is_path_absence_error(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ENOTDIR)
}

fn is_retryable_lock_release_error(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EPERM | libc::EBUSY))
}

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
    if error.raw_os_error() == Some(libc::EPERM) {
        if path_may_exist(lock_path, deps) {
            return Ok(());
        }
        let platform = deps.platform.unwrap_or(std::env::consts::OS);
        if platform == "win32"
            && let Some(parent) = lock_path.parent()
            && path_may_exist(parent, deps)
        {
            return Ok(());
        }
    }
    Err(error)
}

fn is_pid_alive(pid: i64) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: kill with signal 0 only probes for process existence.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn acquire_lock(lock_path: &Path, owner_tag: &str, stale_after_ms: i64) -> Result<()> {
    let started_at = now_ms();
    loop {
        if now_ms() - started_at > LOCK_WAIT_TIMEOUT_MS {
            return Err(TeamCoreError::LockTimeout(lock_path.display().to_string()));
        }
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(mut file) => {
                file.write_all(build_owner_content(owner_tag).as_bytes())?;
                tolerant_fsync(&file, "acquireLock")?;
                return Ok(());
            }
            Err(error) => {
                assert_retryable_lock_open_error(lock_path, error)?;
                if detect_stale_lock(lock_path, stale_after_ms) {
                    reap_stale_lock(lock_path);
                    continue;
                }
                thread::sleep(Duration::from_millis(LOCK_RETRY_MS));
            }
        }
    }
}

/// `withLock`: run `run` while holding the lock file; the lock is released afterwards.
pub fn with_lock<T>(
    lock_path: &Path,
    run: impl FnOnce() -> Result<T>,
    opts: Option<&LockOptions>,
) -> Result<T> {
    let stale_after_ms = opts
        .and_then(|opts| opts.stale_after_ms)
        .unwrap_or(DEFAULT_STALE_AFTER_MS);
    let owner_tag = opts
        .and_then(|opts| opts.owner_tag.as_deref())
        .unwrap_or("owner");
    acquire_lock(lock_path, owner_tag, stale_after_ms)?;
    let result = run();
    reap_stale_lock(lock_path);
    result
}

/// `detectStaleLock`: the owner is dead and the lock is older than `stale_after_ms`.
#[must_use]
pub fn detect_stale_lock(lock_path: &Path, stale_after_ms: i64) -> bool {
    let Ok(content) = fs::read_to_string(lock_path) else {
        return false;
    };
    let Some((owner_pid, acquired_at)) = parse_owner_content(&content) else {
        return false;
    };
    if is_pid_alive(owner_pid) {
        return false;
    }
    now_ms() - acquired_at > stale_after_ms
}

/// `reapStaleLock` with the real filesystem.
pub fn reap_stale_lock(lock_path: &Path) {
    reap_stale_lock_with(lock_path, &LockReleaseDeps::default());
}

/// `reapStaleLock`: unlink, retrying EPERM/EBUSY up to three attempts 25ms apart.
pub fn reap_stale_lock_with(lock_path: &Path, deps: &LockReleaseDeps<'_>) {
    for attempt in 1..=LOCK_RELEASE_RETRY_ATTEMPTS {
        let result = match &deps.unlink {
            Some(unlink) => unlink(lock_path),
            None => fs::remove_file(lock_path),
        };
        let Err(error) = result else {
            return;
        };
        if is_path_absence_error(&error)
            || !is_retryable_lock_release_error(&error)
            || attempt == LOCK_RELEASE_RETRY_ATTEMPTS
        {
            return;
        }
        match &deps.delay {
            Some(delay) => delay(LOCK_RELEASE_RETRY_MS),
            None => thread::sleep(Duration::from_millis(LOCK_RELEASE_RETRY_MS)),
        }
    }
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
