//! Lost-lock reporting and a final write fence. No compromise callback can unwind the owner.
pub type OnLostCallbackState = Arc<Mutex<Option<Arc<dyn Fn(&FileLockLostError) + Send + Sync>>>>;
pub type OnLostCallback = Option<Arc<dyn Fn(&FileLockLostError) + Send + Sync>>;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
#[derive(Debug, Clone, thiserror::Error)]
#[error(
    "File lock on {name} was lost before the write committed: the lock was removed or reclaimed by another process. The write was skipped."
)]
pub struct FileLockLostError {
    pub path: PathBuf,
    pub name: String,
    pub cause: String,
}
impl FileLockLostError {
    pub const CODE: &'static str = "ELOCKLOST";
}
#[derive(Clone)]
pub struct HeldFileLock {
    path: PathBuf,
    token_path: PathBuf,
    token: String,
    released: Arc<AtomicBool>,
    loss: Arc<Mutex<Option<FileLockLostError>>>,
    on_lost: OnLostCallbackState,
}
impl HeldFileLock {
    pub fn new(
        path: &Path,
        lock_dir: &Path,
        on_lost: OnLostCallback,
    ) -> std::io::Result<Self> {
        let token = uuid::Uuid::new_v4().to_string();
        let token_path = lock_dir.join("owner-token");
        std::fs::write(&token_path, &token)?;
        Ok(Self {
            path: path.into(),
            token_path,
            token,
            released: Arc::new(AtomicBool::new(false)),
            loss: Arc::new(Mutex::new(None)),
            on_lost: Arc::new(Mutex::new(on_lost)),
        })
    }
    pub fn start_heartbeat(&self) {
        let held = self.clone();
        std::thread::spawn(move || {
            while !held.released.load(Ordering::SeqCst) && !held.lost() {
                std::thread::park_timeout(std::time::Duration::from_millis(
                    crate::lockfile_policy::FILE_STORAGE_LOCK_UPDATE_MS,
                ));
                if held.released.load(Ordering::SeqCst) {
                    break;
                }
                if held.assert_held().is_err() {
                    break;
                }
                if let Some(dir) = held.token_path.parent()
                    && let Err(error) = filetime::set_file_mtime(dir, filetime::FileTime::now()) {
                        held.report_compromised(error.to_string());
                    }
            }
        });
    }

    pub fn report_compromised(&self, cause: String) {
        if self.released.load(Ordering::SeqCst) {
            return;
        }
        let error = {
            let mut loss = self
                .loss
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if loss.is_some() {
                return;
            }
            let e = FileLockLostError {
                path: self.path.clone(),
                name: self
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                cause,
            };
            *loss = Some(e.clone());
            e
        };
        use sha2::Digest;
        let lock_id = hex::encode(sha2::Sha256::digest(self.path.to_string_lossy().as_bytes()));
        crate::hidden_stdout_log::append_debug_note(&serde_json::json!({"event":"file_lock_lost","lock":error.name,"lockId":&lock_id[..12]}).to_string());
        let callback = self
            .on_lost
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(callback) = callback
            && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(&error))).is_err()
            {
                eprintln!("file_lock_on_lost_threw");
            }
    }
    pub fn lost(&self) -> bool {
        self.loss
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }
    pub fn assert_held(&self) -> Result<(), FileLockLostError> {
        if !self.released.load(Ordering::SeqCst) {
            match std::fs::read_to_string(&self.token_path) {
                Ok(token) if token == self.token => {}
                Ok(_) => self.report_compromised("lock ownership changed".into()),
                Err(e) => self.report_compromised(e.to_string()),
            }
        }
        if self.released.load(Ordering::SeqCst) {
            return Ok(());
        }
        if let Some(e) = self
            .loss
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            Err(e)
        } else {
            Ok(())
        }
    }
    pub fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
    }
    pub fn owns_directory(&self) -> bool {
        std::fs::read_to_string(&self.token_path).is_ok_and(|t| t == self.token)
    }
}
pub async fn lock_with_loss_handling(
    path: &str,
    on_lost: OnLostCallback,
) -> Result<(crate::lockfile_policy::LockGuard, HeldFileLock), String> {
    let guard = crate::lockfile_policy::acquire_lock_async(
        path,
        crate::lockfile_policy::FILE_STORAGE_LOCK_RETRY_BUDGET_MS,
    )
    .await
    .map_err(|e| e.to_string())?;
    let held = guard
        .held_lock()
        .ok_or("broker locks are managed by their host")?;
    *held
        .on_lost
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = on_lost;
    Ok((guard, held))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_race_is_ignored_and_live_loss_is_fenced() {
        let root = tempfile::tempdir().expect("temp");
        let dir = root.path().join("lock");
        std::fs::create_dir(&dir).expect("lock");
        let held = HeldFileLock::new(&root.path().join("data"), &dir, None).expect("held");
        held.release();
        held.report_compromised("late stat".into());
        assert!(!held.lost());
        let live = HeldFileLock::new(&root.path().join("data"), &dir, None).expect("held");
        std::fs::write(dir.join("owner-token"), "new-owner").expect("replace");
        assert_eq!(live.assert_held().expect_err("lost").code(), "ELOCKLOST");
    }
}
impl FileLockLostError {
    pub fn code(&self) -> &'static str {
        Self::CODE
    }
}

impl std::fmt::Debug for HeldFileLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldFileLock")
            .field("path", &self.path)
            .field("lost", &self.lost())
            .finish()
    }
}

#[cfg(test)]
mod write_fence_tests {

    #[test]
    fn lost_lock_does_not_delete_replacement_or_write_data() {
        let root = tempfile::tempdir().expect("temp");
        let path = root.path().join("state");
        std::fs::write(&path, "old").expect("old state");
        let guard = crate::lockfile_policy::acquire_lock_sync(path.to_str().expect("path"), 100)
            .expect("lock");
        std::fs::remove_dir_all(guard.path()).expect("reclaim");
        std::fs::create_dir(guard.path()).expect("replacement");
        std::fs::write(guard.path().join("owner-token"), "new").expect("owner");
        assert!(guard.assert_held().is_err());
        let lock_path = guard.path().to_owned();
        drop(guard);
        assert_eq!(std::fs::read_to_string(path).expect("state"), "old");
        assert_eq!(
            std::fs::read_to_string(lock_path.join("owner-token")).expect("owner"),
            "new"
        );
    }
}
