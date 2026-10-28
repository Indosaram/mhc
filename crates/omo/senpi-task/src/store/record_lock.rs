use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use super::types::StoreError;

const LOCK_RETRY_MIN: Duration = Duration::from_millis(1);
const LOCK_RETRY: Duration = Duration::from_millis(10);
const LOCK_WAIT_TIMEOUT: Duration = Duration::from_millis(1_000);
// Any lock older than this was left by a crashed holder. File age is the staleness authority
// because a pid probe can false-alive after pid reuse; the pid+timestamp body is diagnostic only.
const LOCK_STALE: Duration = Duration::from_millis(5_000);

/// Runs `operation` while holding the `<record>.lock` sibling file.
pub fn with_task_record_lock<T>(
    record_path: &Path,
    operation: impl FnOnce() -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    with_task_record_lock_options(record_path, operation, LOCK_WAIT_TIMEOUT)
}

pub fn with_task_record_lock_options<T>(
    record_path: &Path,
    operation: impl FnOnce() -> Result<T, StoreError>,
    holder_wait: Duration,
) -> Result<T, StoreError> {
    let lock_path = lock_path_for(record_path);
    acquire_lock(&lock_path, holder_wait)?;
    let result = operation();
    remove_if_present(&lock_path)?;
    result
}

fn lock_path_for(record_path: &Path) -> PathBuf {
    let mut name = record_path.as_os_str().to_os_string();
    name.push(".lock");
    PathBuf::from(name)
}

fn acquire_lock(lock_path: &Path, holder_wait: Duration) -> Result<(), StoreError> {
    let started = Instant::now();
    let mut retry = LOCK_RETRY_MIN;
    loop {
        match retry_windows_sharing(|| {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(lock_path)
        }) {
            Ok(mut file) => {
                let now_ms = crate::state::system_now_ms();
                writeln!(file, "{}\n{now_ms}", std::process::id())?;
                return Ok(());
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::AlreadyExists
                    || is_occupied_windows_lock(&error, lock_path) =>
            {
                if is_stale_lock(lock_path) {
                    remove_if_present(lock_path)?;
                    continue;
                }
                if started.elapsed() >= holder_wait {
                    return Err(StoreError::LockTimeout(lock_path.to_path_buf()));
                }
                std::thread::sleep(retry);
                retry = (retry * 2).min(LOCK_RETRY);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn is_occupied_windows_lock(error: &std::io::Error, lock_path: &Path) -> bool {
    if !cfg!(windows) || !is_windows_sharing_error(error) {
        return false;
    }
    // A released lock can remain delete-pending while a metadata/scanner handle closes.
    // CREATE_NEW then reports access denied rather than AlreadyExists. As in upstream
    // publishLock/lockNameOccupied, retry only when the name is still occupied; an actual
    // permission failure on an absent name must propagate.
    match std::fs::metadata(lock_path) {
        Ok(_) => true,
        Err(error) => is_windows_sharing_error(&error),
    }
}

fn is_windows_sharing_error(error: &std::io::Error) -> bool {
    cfg!(windows)
        && (error.kind() == std::io::ErrorKind::PermissionDenied
            || matches!(error.raw_os_error(), Some(32 | 33)))
}

// Pinned record-write.ts retries Windows EPERM/EBUSY/EACCES eight times at 5 ms.
// Retry the syscall, not its caller: a committed write must never be replayed, and a
// persistent permission error must retain its original IO error rather than become success.
pub(crate) fn retry_windows_sharing<T>(
    mut operation: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    let mut retries = 0;
    loop {
        match operation() {
            Err(error) if retries < 8 && is_windows_sharing_error(&error) => {
                retries += 1;
                std::thread::sleep(Duration::from_millis(5));
            }
            result => return result,
        }
    }
}

fn is_stale_lock(lock_path: &Path) -> bool {
    match std::fs::metadata(lock_path).and_then(|metadata| metadata.modified()) {
        Ok(modified) => SystemTime::now()
            .duration_since(modified)
            .is_ok_and(|age| age > LOCK_STALE),
        // A lock that vanished between the create attempt and this stat was released.
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

pub(crate) fn remove_if_present(path: &Path) -> Result<(), StoreError> {
    match retry_windows_sharing(|| std::fs::remove_file(path)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
