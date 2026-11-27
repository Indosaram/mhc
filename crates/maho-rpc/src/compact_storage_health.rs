//! senpi `modes/rpc/compact-storage-health.ts`: shared durable storage-health clock
//! (RULING-84), independent of accepted sequence and freeze generation.

use crate::compact_read_lease::{CompactError, CompactReadReason};
use crate::compact_root_owner::private_root_stat;
use crate::ownership_safe_lock::{LockRetries, acquire_ownership_safe_lock};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// Standalone stores have no host defaults; capture their fallback at startup, never per session.
pub fn recovery_agent_dir() -> PathBuf {
    PathBuf::from(maho_core::config::get_agent_dir())
}

fn dir_suffix() -> String {
    #[cfg(unix)]
    {
        rustix::process::getuid().as_raw().to_string()
    }
    #[cfg(not(unix))]
    {
        "user".to_owned()
    }
}

fn is_canonical_revision(text: &str) -> bool {
    text == "0"
        || (!text.is_empty() && !text.starts_with('0') && text.bytes().all(|b| b.is_ascii_digit()))
}

pub struct RecoveryStorageHealth {
    dir: PathBuf,
    failures: AtomicU64,
    cleared: AtomicU64,
    repair_lock: tokio::sync::Mutex<()>,
    setup_done: Mutex<bool>,
}

impl RecoveryStorageHealth {
    pub fn new(base: &Path) -> Self {
        Self {
            dir: base.join(format!(".senpi-recovery-health-{}", dir_suffix())),
            failures: AtomicU64::new(0),
            cleared: AtomicU64::new(0),
            repair_lock: tokio::sync::Mutex::new(()),
            setup_done: Mutex::new(false),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn needs_recovery(&self) -> bool {
        self.failures.load(Ordering::SeqCst) > self.cleared.load(Ordering::SeqCst)
    }

    pub fn unavailable(&self) {
        self.failures.fetch_add(1, Ordering::SeqCst);
    }

    async fn ensure_dir(&self) -> Result<(), CompactError> {
        if *self
            .setup_done
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            return Ok(());
        }
        let _guard = self.repair_lock.lock().await;
        if *self
            .setup_done
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            return Ok(());
        }
        tokio::fs::create_dir_all(&self.dir)
            .await
            .map_err(|e| CompactError::Io(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                tokio::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700)).await;
        }
        let stat = tokio::fs::symlink_metadata(&self.dir)
            .await
            .map_err(|e| CompactError::Io(e.to_string()))?;
        if !private_root_stat(&stat) {
            return Err(CompactError::read(CompactReadReason::Ownership));
        }
        *self
            .setup_done
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = true;
        Ok(())
    }

    pub async fn revision(&self) -> Result<String, CompactError> {
        self.ensure_dir().await?;
        match self.read_revision().await? {
            Some(rev) => Ok(rev),
            None => {
                let _guard = self.repair_lock.lock().await;
                if let Some(rev) = self.read_revision().await? {
                    return Ok(rev);
                }
                self.advance().await
            }
        }
    }

    async fn read_revision(&self) -> Result<Option<String>, CompactError> {
        let rev_path = self.dir.join("revision");
        let mut opts = tokio::fs::OpenOptions::new();
        opts.read(true);
        #[cfg(unix)]
        {
            opts.custom_flags(libc::O_NOFOLLOW);
        }
        let file = match opts.open(&rev_path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Some("0".into())),
            #[cfg(unix)]
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return Ok(None),
            Err(e) => return Err(CompactError::Io(e.to_string())),
        };
        let stat = file
            .metadata()
            .await
            .map_err(|e| CompactError::Io(e.to_string()))?;
        if !stat.is_file() || stat.len() > 128 {
            return Ok(None);
        }
        let mut text = String::new();
        use tokio::io::AsyncReadExt;
        let mut reader = file;
        reader
            .read_to_string(&mut text)
            .await
            .map_err(|e| CompactError::Io(e.to_string()))?;
        let trimmed = text.trim();
        Ok(is_canonical_revision(trimmed).then(|| trimmed.to_owned()))
    }

    /// The operation must actually finish successfully; a refusal leaves both failure and revision unchanged.
    pub async fn recover<F, Fut>(&self, operation: F) -> Result<String, CompactError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(), CompactError>>,
    {
        self.ensure_dir().await?;
        let through = self.failures.load(Ordering::SeqCst);
        operation().await?;
        if through <= self.cleared.load(Ordering::SeqCst) {
            return self.revision().await;
        }
        let _guard = self.repair_lock.lock().await;
        if through <= self.cleared.load(Ordering::SeqCst) {
            return self.revision().await;
        }
        let revision = self.advance().await?;
        self.cleared.store(through, Ordering::SeqCst);
        Ok(revision)
    }

    async fn advance(&self) -> Result<String, CompactError> {
        self.ensure_dir().await?;
        let lock_path = self.dir.join("revision.lock");
        let mut lock = acquire_ownership_safe_lock(&lock_path, LockRetries::default())
            .await
            .map_err(|e| CompactError::Io(e.to_string()))?;
        let temporary = self.dir.join(format!("{}.tmp", uuid::Uuid::new_v4()));

        let write_result: Result<String, CompactError> = async {
            let current = self.read_revision().await?.unwrap_or_else(|| "0".into());
            let parsed = current.parse::<u128>().unwrap_or(0);
            let next = (parsed + 1).to_string();

            use tokio::io::AsyncWriteExt;
            let mut opts = tokio::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                opts.mode(0o600);
                opts.custom_flags(libc::O_NOFOLLOW);
            }
            let mut file = opts
                .open(&temporary)
                .await
                .map_err(|e| CompactError::Io(e.to_string()))?;
            file.write_all(next.as_bytes())
                .await
                .map_err(|e| CompactError::Io(e.to_string()))?;
            file.sync_all()
                .await
                .map_err(|e| CompactError::Io(e.to_string()))?;
            drop(file);

            tokio::fs::rename(&temporary, self.dir.join("revision"))
                .await
                .map_err(|e| CompactError::Io(e.to_string()))?;
            if let Ok(dir_file) = tokio::fs::File::open(&self.dir).await {
                let _ = dir_file.sync_all().await;
            }
            Ok(next)
        }
        .await;

        let _ = tokio::fs::remove_file(&temporary).await;
        let _ = lock.release();
        write_result
    }
}

pub type SharedStorageHealth = Arc<RecoveryStorageHealth>;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn revision_starts_at_zero_and_recover_advances_after_success_only() {
        let base = tempfile::tempdir().expect("tempdir");
        let health = RecoveryStorageHealth::new(base.path());
        assert!(!health.needs_recovery());
        assert_eq!(health.revision().await.expect("rev 0"), "0");

        health.unavailable();
        assert!(health.needs_recovery());

        let err = health
            .recover(|| async { Err(CompactError::Io("disk down".into())) })
            .await
            .expect_err("failing op leaves failure state");
        assert!(matches!(err, CompactError::Io(_)));
        assert!(health.needs_recovery());
        assert_eq!(health.revision().await.expect("still 0"), "0");

        let rev = health
            .recover(|| async { Ok(()) })
            .await
            .expect("recovered");
        assert_eq!(rev, "1");
        assert!(!health.needs_recovery());
    }
}
