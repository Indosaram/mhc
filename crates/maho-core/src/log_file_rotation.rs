//! Port of senpi packages/coding-agent/src/core/log-file-rotation.ts.

use std::fs::OpenOptions;
use std::io;
use std::time::{Duration, SystemTime};

/// A log sink that failed waits this long before it tries the file again, so it recovers.
pub const LOG_SINK_RETRY_MS: u64 = 5_000;

const ROTATION_LOCK_STALE_MS: u64 = 10_000;

fn exceeds_cap(file_path: &str, incoming_bytes: u64, max_bytes: u64) -> io::Result<bool> {
    match std::fs::metadata(file_path) {
        Ok(metadata) => Ok(metadata.len() + incoming_bytes > max_bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn clear_stale_lock(lock_path: &str) -> io::Result<()> {
    match std::fs::metadata(lock_path) {
        Ok(metadata) => {
            let age = metadata
                .modified()
                .ok()
                .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                .unwrap_or(Duration::ZERO);
            if age > Duration::from_millis(ROTATION_LOCK_STALE_MS) {
                let _ = std::fs::remove_file(lock_path);
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn private_mode(path: &str) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn private_mode(_path: &str) -> io::Result<()> {
    Ok(())
}

/// Rotates `file_path` to `file_path.1` once appending `incoming_bytes` would pass `max_bytes`. Several
/// processes append to one log, so the rotation runs under an exclusive lock file and re-checks the size
/// while holding it (senpi#2976). A held lock skips rotation for this line; a lock left by a crashed process
/// is cleared once it is stale.
pub fn rotate_log_if_needed(
    file_path: &str,
    incoming_bytes: u64,
    max_bytes: u64,
) -> io::Result<()> {
    if !exceeds_cap(file_path, incoming_bytes, max_bytes)? {
        return Ok(());
    }
    let lock_path = format!("{file_path}.rotate-lock");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = match options.open(&lock_path) {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return clear_stale_lock(&lock_path);
        }
        Err(error) => return Err(error),
    };
    let rotated = (|| {
        if !exceeds_cap(file_path, incoming_bytes, max_bytes)? {
            return Ok(());
        }
        let generation = format!("{file_path}.1");
        match std::fs::rename(file_path, &generation) {
            Ok(()) => private_mode(&generation),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    })();
    drop(lock);
    let _ = std::fs::remove_file(&lock_path);
    rotated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_only_past_the_cap_and_skips_while_another_process_holds_the_lock() {
        let Some(dir) = tempfile::tempdir().ok() else {
            panic!("tempdir")
        };
        let log = dir.path().join("app.log").to_string_lossy().into_owned();
        assert!(rotate_log_if_needed(&log, 10, 5).is_ok());
        assert!(std::fs::write(&log, "12345").is_ok());
        assert!(rotate_log_if_needed(&log, 5, 10).is_ok());
        assert!(std::path::Path::new(&log).exists());
        assert!(std::fs::write(format!("{log}.rotate-lock"), "").is_ok());
        assert!(rotate_log_if_needed(&log, 6, 10).is_ok());
        assert!(std::path::Path::new(&log).exists());
        let _ = std::fs::remove_file(format!("{log}.rotate-lock"));
        assert!(rotate_log_if_needed(&log, 6, 10).is_ok());
        assert!(!std::path::Path::new(&log).exists());
        assert_eq!(
            std::fs::read_to_string(format!("{log}.1")).ok().as_deref(),
            Some("12345")
        );
        assert!(!std::path::Path::new(&format!("{log}.rotate-lock")).exists());
    }
}
