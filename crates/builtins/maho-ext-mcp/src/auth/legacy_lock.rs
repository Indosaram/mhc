//! Port of senpi `auth/legacy-lock.ts`: an async O_EXCL lock file shared with the
//! synchronous legacy-token migrators, without blocking the runtime.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Held lock: dropping it without `release` leaves the file for the stale sweep, matching an
/// upstream process that died while holding it.
pub struct LegacyLockGuard {
    path: PathBuf,
    file: Option<std::fs::File>,
}

impl LegacyLockGuard {
    pub async fn release(mut self) -> std::io::Result<()> {
        drop(self.file.take());
        remove_force(&self.path).await
    }
}

async fn remove_force(path: &Path) -> std::io::Result<()> {
    match tokio::fs::remove_file(path).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn open_exclusive(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn age_ms(modified: SystemTime) -> f64 {
    SystemTime::now()
        .duration_since(modified)
        .map_or(0.0, |age| age.as_secs_f64() * 1000.0)
}

/// `acquireLegacyLock(path, stale)`: retries every 20 ms, breaks a lock older than `stale` ms,
/// and fails once `stale` ms have passed without acquiring it.
pub async fn acquire_legacy_lock(path: &Path, stale_ms: u64) -> std::io::Result<LegacyLockGuard> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(stale_ms);
    loop {
        match open_exclusive(path) {
            Ok(file) => {
                return Ok(LegacyLockGuard {
                    path: path.to_path_buf(),
                    file: Some(file),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        match tokio::fs::metadata(path).await {
            Ok(metadata) => {
                if age_ms(metadata.modified()?) >= stale_ms as f64 {
                    remove_force(path).await?;
                    continue;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(std::io::Error::other(
                "Legacy OAuth migration lock acquisition timed out",
            ));
        }
        crate::wrap::safe_delay(Duration::from_millis(20)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn acquire_release_and_reacquire() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("lock");
        let guard = acquire_legacy_lock(&path, 1_000).await?;
        assert!(path.exists());
        guard.release().await?;
        assert!(!path.exists());
        acquire_legacy_lock(&path, 1_000).await?.release().await
    }

    #[tokio::test]
    async fn stale_lock_is_broken() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("lock");
        let _held = acquire_legacy_lock(&path, 60_000).await?;
        // A zero stale window treats every existing lock as abandoned and breaks it.
        acquire_legacy_lock(&path, 0).await?.release().await
    }

    #[tokio::test]
    async fn live_lock_inside_window_reports_timeout() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("lock");
        std::fs::write(&path, b"")?;
        let file = std::fs::File::options().write(true).open(&path)?;
        file.set_modified(SystemTime::now() + Duration::from_secs(3600))?;
        let error = acquire_legacy_lock(&path, 50).await.err().expect("timeout");
        assert_eq!(
            error.to_string(),
            "Legacy OAuth migration lock acquisition timed out"
        );
        Ok(())
    }
}
