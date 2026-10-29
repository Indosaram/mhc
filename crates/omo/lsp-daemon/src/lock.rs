//! Port of `lock.ts`: exclusive pid lock files with stale-owner reclamation.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::platform;

/// Held lock; released by `release` (TS `LockHandle`).
#[derive(Debug)]
pub struct LockHandle {
    path: PathBuf,
}

impl LockHandle {
    pub fn release(&self) {
        unlink_quietly(&self.path);
    }
}

/// TS `isProcessAlive`: `kill(pid, 0)` succeeds or fails with EPERM.
pub fn is_process_alive(pid: i64) -> bool {
    u32::try_from(pid).is_ok_and(|pid| pid > 0 && platform::process_alive(pid))
}

/// TS `readLockPid`: leading integer of the lock file, or `None`.
pub fn read_lock_pid(lock_path: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(lock_path).ok()?;
    parse_leading_int(text.trim())
}

/// Mirrors `Number.parseInt(text, 10)` for the digits this file format uses.
pub(crate) fn parse_leading_int(text: &str) -> Option<i64> {
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, text.strip_prefix('+').unwrap_or(text)),
    };
    let end = digits
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(digits.len());
    digits[..end].parse::<i64>().ok().map(|value| sign * value)
}

/// TS `tryAcquireLock`: create the lock exclusively; reap a dead owner's lock once.
pub fn try_acquire_lock(lock_path: &Path, owner_pid: u32) -> io::Result<Option<LockHandle>> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    for _attempt in 0..2 {
        if let Some(handle) = write_lock_file(lock_path, owner_pid)? {
            return Ok(Some(handle));
        }
        if !reap_stale_lock(lock_path) {
            return Ok(None);
        }
    }
    Ok(None)
}

fn write_lock_file(lock_path: &Path, owner_pid: u32) -> io::Result<Option<LockHandle>> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(lock_path) {
        Ok(mut file) => {
            file.write_all(format!("{owner_pid}\n").as_bytes())?;
            Ok(Some(LockHandle {
                path: lock_path.to_path_buf(),
            }))
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(None),
        Err(error) => Err(error),
    }
}

fn reap_stale_lock(lock_path: &Path) -> bool {
    if read_lock_pid(lock_path).is_some_and(is_process_alive) {
        return false;
    }
    unlink_quietly(lock_path);
    true
}

/// TS `unlinkQuietly`: remove a file, ignoring every failure.
pub fn unlink_quietly(path: &Path) {
    let _ignored = std::fs::remove_file(path);
}

#[cfg(test)]
#[path = "lock_tests.rs"]
mod tests;
