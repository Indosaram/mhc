//! Port of senpi packages/coding-agent/src/core/lockfile-policy.ts.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const FILE_STORAGE_LOCK_STALE_MS: u64 = 30_000;
pub const FILE_STORAGE_LOCK_UPDATE_MS: u64 = 10_000;
pub const FILE_STORAGE_LOCK_RETRY_BUDGET_MS: u64 = 5_500;
pub const FILE_STORAGE_LOCK_RETRY_MIN_DELAY_MS: u64 = 100;
pub const FILE_STORAGE_LOCK_RETRY_MAX_DELAY_MS: u64 = 1_000;
pub const FILE_STORAGE_SYNC_LOCK_BUDGET_MS: u64 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialStoreBusyError {
    pub path: String,
    pub waited_ms: u64,
}

impl CredentialStoreBusyError {
    pub fn new(path: impl Into<String>, waited_ms: u64) -> Self {
        Self { path: path.into(), waited_ms }
    }

    pub fn message(&self) -> String {
        format!(
            "Credential store is busy: lock {} was held for {}ms. Another omo process may be refreshing credentials; close unused sessions if contention persists.",
            self.path, self.waited_ms
        )
    }
}

impl std::fmt::Display for CredentialStoreBusyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for CredentialStoreBusyError {}

pub fn is_lock_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
}

fn lock_dir(path: &str) -> PathBuf {
    let mut name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    name.push_str(".lock");
    Path::new(path).with_file_name(name)
}

pub struct LockGuard {
    dir: PathBuf,
}

impl LockGuard {
    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn try_acquire(dir: &Path) -> std::io::Result<()> {
    match std::fs::create_dir(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if let Ok(metadata) = std::fs::metadata(dir) {
                if let Ok(modified) = metadata.modified() {
                    if modified.elapsed().map(|age| age.as_millis() as u64 > FILE_STORAGE_LOCK_STALE_MS).unwrap_or(false) {
                        let _ = std::fs::remove_dir_all(dir);
                        return std::fs::create_dir(dir);
                    }
                }
            }
            Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "ELOCKED"))
        }
        Err(error) => Err(error),
    }
}

pub fn acquire_lock_sync(path: &str, budget_ms: u64) -> Result<LockGuard, CredentialStoreBusyError> {
    let dir = lock_dir(path);
    if let Some(parent) = dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let started = Instant::now();
    let mut delay_ms = FILE_STORAGE_LOCK_RETRY_MIN_DELAY_MS;
    loop {
        match try_acquire(&dir) {
            Ok(()) => return Ok(LockGuard { dir }),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let waited = started.elapsed().as_millis() as u64;
                if waited >= budget_ms {
                    return Err(CredentialStoreBusyError::new(path, waited));
                }
                std::thread::sleep(Duration::from_millis(delay_ms.min(budget_ms.saturating_sub(waited)).max(1)));
                delay_ms = (delay_ms * 2).min(FILE_STORAGE_LOCK_RETRY_MAX_DELAY_MS);
            }
            Err(error) => {
                return Err(CredentialStoreBusyError::new(format!("{path}: {error}"), started.elapsed().as_millis() as u64));
            }
        }
    }
}

pub async fn acquire_lock_async(path: &str, budget_ms: u64) -> Result<LockGuard, CredentialStoreBusyError> {
    let dir = lock_dir(path);
    if let Some(parent) = dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let started = Instant::now();
    let mut delay_ms = FILE_STORAGE_LOCK_RETRY_MIN_DELAY_MS;
    loop {
        match try_acquire(&dir) {
            Ok(()) => return Ok(LockGuard { dir }),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let waited = started.elapsed().as_millis() as u64;
                if waited >= budget_ms {
                    return Err(CredentialStoreBusyError::new(path, waited));
                }
                tokio::time::sleep(Duration::from_millis(delay_ms.min(budget_ms.saturating_sub(waited)).max(1))).await;
                delay_ms = (delay_ms * 2).min(FILE_STORAGE_LOCK_RETRY_MAX_DELAY_MS);
            }
            Err(error) => {
                return Err(CredentialStoreBusyError::new(format!("{path}: {error}"), started.elapsed().as_millis() as u64));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquires_and_releases_a_lock() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("auth.json");
        std::fs::write(&target, "{}").expect("write");
        let path = target.to_string_lossy().into_owned();
        let guard = acquire_lock_sync(&path, FILE_STORAGE_SYNC_LOCK_BUDGET_MS).expect("lock");
        assert!(guard.path().exists());
        let dir = lock_dir(&path);
        drop(guard);
        assert!(!dir.exists());
    }

    #[test]
    fn reports_a_busy_store_when_the_budget_elapses() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("auth.json");
        std::fs::write(&target, "{}").expect("write");
        let path = target.to_string_lossy().into_owned();
        let _held = acquire_lock_sync(&path, FILE_STORAGE_SYNC_LOCK_BUDGET_MS).expect("lock");
        let error = acquire_lock_sync(&path, 50).expect_err("busy");
        assert!(error.message().starts_with("Credential store is busy: lock "));
        assert!(error.message().contains("was held for"));
    }
}
