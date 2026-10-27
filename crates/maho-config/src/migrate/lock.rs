//! The migration lock shared with other tools using `~/.omo` (spec §12.1).
//!
//! Interface fact: the lock file holds one JSON line `{"leaseExpiresAt": <ms>, "pid": <int>}`.

use super::MigrationEnv;
use maho_support::{write_atomically, write_exclusive};
use serde_json::{Value, json};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default lease of the main lock.
pub const LEASE_MS: i64 = 30_000;
/// Lease of the reclaim guard.
pub const GUARD_LEASE_MS: i64 = 1_000;
/// Acquisition attempts before giving up.
pub const ATTEMPTS: usize = 3;
const GUARD_BACKOFF_MS: [u64; 5] = [2, 4, 8, 16, 32];

/// A lock record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LockRecord {
    pub lease_expires_at: i64,
    pub pid: i64,
}

impl LockRecord {
    pub fn to_line(self) -> String {
        format!(
            "{}\n",
            json!({"leaseExpiresAt": self.lease_expires_at, "pid": self.pid})
        )
    }

    pub fn parse(text: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(text.trim()).ok()?;
        Some(Self {
            lease_expires_at: value.get("leaseExpiresAt")?.as_f64()? as i64,
            pid: value.get("pid")?.as_i64()?,
        })
    }
}

/// A held lock.
pub struct HeldLock {
    pub path: PathBuf,
    content: String,
}

fn guard_path(lock: &Path) -> PathBuf {
    PathBuf::from(format!("{}.guard", lock.display()))
}

fn reclaimable(text: &str, env: &MigrationEnv, lease_ms: i64) -> bool {
    let Some(record) = LockRecord::parse(text) else {
        return true;
    };
    let now = (env.now_ms)();
    let expired = now >= record.lease_expires_at;
    expired && (!(env.is_alive)(record.pid) || now > record.lease_expires_at + 2 * lease_ms)
}

/// Removes `path` only when its content is still exactly `observed`.
fn remove_if_content(path: &Path, observed: &str) -> io::Result<bool> {
    match std::fs::read_to_string(path) {
        Ok(current) if current == observed => std::fs::remove_file(path).map(|()| true),
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn with_guard(
    lock: &Path,
    env: &MigrationEnv,
    action: impl FnOnce() -> io::Result<()>,
) -> io::Result<bool> {
    let guard = guard_path(lock);
    for (index, backoff) in std::iter::once(0).chain(GUARD_BACKOFF_MS).enumerate() {
        if index > 0 {
            std::thread::sleep(Duration::from_millis(backoff));
        }
        let record = LockRecord {
            lease_expires_at: (env.now_ms)() + GUARD_LEASE_MS,
            pid: env.pid,
        }
        .to_line();
        match write_exclusive(&guard, record.as_bytes()) {
            Ok(()) => {
                let result = action();
                let _ = remove_if_content(&guard, &record);
                return result.map(|()| true);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if let Ok(text) = std::fs::read_to_string(&guard)
                    && reclaimable(&text, env, GUARD_LEASE_MS)
                {
                    let _ = remove_if_content(&guard, &text);
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

/// Tries to take the lock; `Ok(None)` when another live holder keeps it.
pub fn acquire(lock: &Path, env: &MigrationEnv) -> io::Result<Option<HeldLock>> {
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent)?;
    }
    for _ in 0..ATTEMPTS {
        let content = LockRecord {
            lease_expires_at: (env.now_ms)() + env.lease_ms,
            pid: env.pid,
        }
        .to_line();
        match write_exclusive(lock, content.as_bytes()) {
            Ok(()) => {
                return Ok(Some(HeldLock {
                    path: lock.to_path_buf(),
                    content,
                }));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let observed = match std::fs::read_to_string(lock) {
                    Ok(text) => text,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                };
                if reclaimable(&observed, env, env.lease_ms) {
                    with_guard(lock, env, || remove_if_content(lock, &observed).map(|_| ()))?;
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

impl HeldLock {
    /// Extends the lease (before each file operation).
    pub fn renew(&mut self, env: &MigrationEnv) -> io::Result<()> {
        let current = std::fs::read_to_string(&self.path)?;
        if current != self.content {
            return Err(io::Error::other(
                "the migration lock was taken over by another process",
            ));
        }
        let content = LockRecord {
            lease_expires_at: (env.now_ms)() + env.lease_ms,
            pid: env.pid,
        }
        .to_line();
        write_atomically(&self.path, content.as_bytes())?;
        self.content = content;
        Ok(())
    }

    /// Releases the lock if it is still ours (also done on drop).
    pub fn release(self) {}
}

impl Drop for HeldLock {
    fn drop(&mut self) {
        let _ = remove_if_content(&self.path, &self.content);
    }
}
