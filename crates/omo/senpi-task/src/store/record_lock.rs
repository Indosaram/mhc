use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use super::types::StoreError;

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
    let lock_path = lock_path_for(record_path);
    acquire_lock(&lock_path)?;
    let result = operation();
    remove_if_present(&lock_path)?;
    result
}

fn lock_path_for(record_path: &Path) -> PathBuf {
    let mut name = record_path.as_os_str().to_os_string();
    name.push(".lock");
    PathBuf::from(name)
}

fn acquire_lock(lock_path: &Path) -> Result<(), StoreError> {
    let started = Instant::now();
    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(mut file) => {
                let now_ms = crate::state::system_now_ms();
                writeln!(file, "{}\n{now_ms}", std::process::id())?;
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if is_stale_lock(lock_path) {
                    remove_if_present(lock_path)?;
                    continue;
                }
                if started.elapsed() >= LOCK_WAIT_TIMEOUT {
                    return Err(StoreError::LockTimeout(lock_path.to_path_buf()));
                }
                std::thread::sleep(LOCK_RETRY);
            }
            Err(error) => return Err(error.into()),
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
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
