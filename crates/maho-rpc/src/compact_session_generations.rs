//! senpi `modes/rpc/compact-session-generations.ts`: durable monotonic generation reservation
//! (RULING-99). A lifetime kernel lock excludes concurrent owners; synchronous freezes issue only
//! reserved values, preserving cut/suffix ordering.

use crate::compact_read_lease::{CompactError, CompactReadReason, lock};
use crate::ownership_safe_lock::{LockRetries, OwnershipSafeLock, acquire_ownership_safe_lock};
use sha2::{Digest, Sha256};
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

const RANGE: u64 = 1024;
/// JS `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_GENERATION: u64 = 9_007_199_254_740_991;

/// Mirrors upstream `compact-root-owner.ts` `privateRootStat`.
pub fn private_root_stat(metadata: &Metadata) -> bool {
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let uid = rustix::process::getuid().as_raw();
        metadata.uid() == uid && (metadata.permissions().mode() & 0o077) == 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn generations_dir(agent_dir: &Path) -> PathBuf {
    #[cfg(unix)]
    let suffix = rustix::process::getuid().as_raw().to_string();
    #[cfg(not(unix))]
    let suffix = "user".to_owned();
    agent_dir.join(format!(".senpi-recovery-generations-{suffix}"))
}

async fn sync_directory(path: &Path) -> Result<(), CompactError> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let dir = std::fs::File::open(&path).map_err(|e| CompactError::Io(e.to_string()))?;
        dir.sync_all().map_err(|e| CompactError::Io(e.to_string()))
    })
    .await
    .map_err(|e| CompactError::Io(e.to_string()))?
}

fn parse_floor(text: &str) -> Result<u64, CompactError> {
    let valid_syntax = text == "0"
        || (!text.is_empty() && !text.starts_with('0') && text.bytes().all(|b| b.is_ascii_digit()));
    if !valid_syntax {
        return Err(CompactError::Io("compact read refused (integrity)".into()));
    }
    let floor: u64 = text
        .parse()
        .map_err(|_| CompactError::Io("compact read refused (integrity)".into()))?;
    if floor > MAX_SAFE_GENERATION {
        return Err(CompactError::Io("compact read refused (integrity)".into()));
    }
    Ok(floor)
}

#[derive(Default)]
struct InnerState {
    issued: u64,
    ceiling: u64,
    failed: bool,
    closed: bool,
    binding: usize,
    path: Option<PathBuf>,
    lock: Option<OwnershipSafeLock>,
}

/// Monotonic session generation counter with durable range reservations (RULING-99).
pub struct SessionGenerations {
    dir: PathBuf,
    agent_dir: PathBuf,
    state: Arc<Mutex<InnerState>>,
    pending: tokio::sync::Mutex<()>,
    refills: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl SessionGenerations {
    pub fn new(agent_dir: impl AsRef<Path>) -> Self {
        let agent_dir = agent_dir.as_ref().to_path_buf();
        let dir = generations_dir(&agent_dir);
        Self {
            dir,
            agent_dir,
            state: Arc::new(Mutex::new(InnerState {
                ceiling: MAX_SAFE_GENERATION,
                ..InnerState::default()
            })),
            pending: tokio::sync::Mutex::new(()),
            refills: Mutex::new(Vec::new()),
        }
    }

    /// Replacement burns the old range and reserves above both the lane ceiling and the target's
    /// durable floor.
    pub async fn bind(&self, session_id: &str) {
        let digest = hex::encode(Sha256::digest(session_id.as_bytes()));
        let path = self.dir.join(digest);
        {
            let mut state = lock(&self.state);
            if state.closed || state.path.as_ref() == Some(&path) {
                return;
            }
            state.binding += 1;
        }
        let _guard = self.pending.lock().await;
        let result = self.bind_inner(&path).await;
        let mut state = lock(&self.state);
        if result.is_err() {
            state.failed = true;
        }
        state.binding = state.binding.saturating_sub(1);
    }

    async fn bind_inner(&self, path: &Path) -> Result<(), CompactError> {
        let existing_lock = lock(&self.state).lock.take();
        if let Some(mut old_lock) = existing_lock {
            let _ = old_lock.release();
        }
        {
            let mut state = lock(&self.state);
            let previous = state.path.replace(path.to_path_buf());
            state.failed = false;
            if previous.is_some() {
                let floor = state.issued.max(state.ceiling);
                state.issued = floor;
                state.ceiling = floor;
            } else {
                state.ceiling = state.issued;
            }
        }
        self.initialize(path).await
    }

    async fn initialize(&self, path: &Path) -> Result<(), CompactError> {
        let dir = self.dir.clone();
        tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = std::fs::DirBuilder::new();
                builder.recursive(true).mode(0o700);
                builder
                    .create(&dir)
                    .map_err(|e| CompactError::Io(e.to_string()))?;
            }
            #[cfg(not(unix))]
            {
                std::fs::create_dir_all(&dir).map_err(|e| CompactError::Io(e.to_string()))?;
            }
            let stat =
                std::fs::symlink_metadata(&dir).map_err(|e| CompactError::Io(e.to_string()))?;
            if !private_root_stat(&stat) {
                return Err(CompactError::read(CompactReadReason::Ownership));
            }
            Ok(())
        })
        .await
        .map_err(|e| CompactError::Io(e.to_string()))??;
        sync_directory(&self.agent_dir).await?;
        let lock_path = PathBuf::from(format!("{}.lock", path.display()));
        let acquired = acquire_ownership_safe_lock(
            &lock_path,
            LockRetries {
                retries: 0,
                min_timeout: Duration::ZERO,
                max_timeout: Duration::ZERO,
            },
        )
        .await
        .map_err(|e| CompactError::Io(e.to_string()))?;
        lock(&self.state).lock = Some(acquired);

        let read_path = path.to_path_buf();
        let floor = tokio::task::spawn_blocking(move || -> Result<Option<u64>, CompactError> {
            let mut opts = std::fs::OpenOptions::new();
            opts.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.custom_flags(libc::O_NOFOLLOW);
            }
            let file = match opts.open(&read_path) {
                Ok(file) => file,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(CompactError::Io(err.to_string())),
            };
            let meta = file
                .metadata()
                .map_err(|e| CompactError::Io(e.to_string()))?;
            if !meta.is_file() || meta.len() > 32 {
                return Err(CompactError::Io("compact read refused (integrity)".into()));
            }
            let mut text = String::new();
            use std::io::Read;
            let mut file = file;
            file.read_to_string(&mut text)
                .map_err(|e| CompactError::Io(e.to_string()))?;
            Ok(Some(parse_floor(&text)?))
        })
        .await
        .map_err(|e| CompactError::Io(e.to_string()))??;

        if let Some(floor) = floor {
            let mut state = lock(&self.state);
            let value = state.issued.max(floor);
            state.issued = value;
            state.ceiling = value;
        }
        Self::reserve_with(&self.dir, &self.state, path).await
    }

    async fn reserve_with(
        dir: &Path,
        state_arc: &Arc<Mutex<InnerState>>,
        path: &Path,
    ) -> Result<(), CompactError> {
        let ceiling = lock(state_arc).ceiling;
        let next = MAX_SAFE_GENERATION.min(ceiling.saturating_add(RANGE));
        if next <= ceiling {
            return Err(CompactError::Io(
                "compact read refused (unavailable)".into(),
            ));
        }
        let temp = PathBuf::from(format!("{}.{}.tmp", path.display(), Uuid::new_v4()));
        let write_temp = temp.clone();
        let target = path.to_path_buf();
        let write_res = tokio::task::spawn_blocking(move || -> Result<(), CompactError> {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut file = opts
                .open(&write_temp)
                .map_err(|e| CompactError::Io(e.to_string()))?;
            use std::io::Write;
            let res = file
                .write_all(next.to_string().as_bytes())
                .and_then(|()| file.sync_all())
                .map_err(|e| CompactError::Io(e.to_string()));
            drop(file);
            if res.is_err() {
                let _ = std::fs::remove_file(&write_temp);
                return res;
            }
            if let Err(err) = std::fs::rename(&write_temp, &target) {
                let _ = std::fs::remove_file(&write_temp);
                return Err(CompactError::Io(err.to_string()));
            }
            Ok(())
        })
        .await
        .map_err(|e| CompactError::Io(e.to_string()))?;
        write_res?;
        sync_directory(dir).await?;
        lock(state_arc).ceiling = next;
        Ok(())
    }

    /// Exhaustion while refill is pending, corruption, contention and failed persistence all refuse
    /// issuance.
    pub fn next(&self) -> Option<u64> {
        let (generation, refill_path) = {
            let mut state = lock(&self.state);
            if state.closed || state.failed || state.binding > 0 || state.issued >= state.ceiling {
                return None;
            }
            state.issued += 1;
            let generation = state.issued;
            let refill_path = if state.ceiling.saturating_sub(generation) == RANGE / 2 {
                state.path.clone()
            } else {
                None
            };
            (generation, refill_path)
        };
        if let Some(path) = refill_path
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            let dir = self.dir.clone();
            let state = Arc::clone(&self.state);
            let task = handle.spawn(async move {
                if Self::reserve_with(&dir, &state, &path).await.is_err() {
                    lock(&state).failed = true;
                }
            });
            lock(&self.refills).push(task);
        }
        Some(generation)
    }

    pub async fn idle(&self) {
        let _guard = self.pending.lock().await;
        let handles: Vec<_> = lock(&self.refills).drain(..).collect();
        for handle in handles {
            let _ = handle.await;
        }
    }

    pub fn close(&self) {
        let mut state = lock(&self.state);
        if state.closed {
            return;
        }
        state.closed = true;
        if let Some(mut held) = state.lock.take() {
            let _ = held.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_generations_increment_synchronously_until_close() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gens = SessionGenerations::new(dir.path());
        assert_eq!(gens.next(), Some(1));
        assert_eq!(gens.next(), Some(2));
        gens.close();
        assert_eq!(gens.next(), None);
    }

    #[tokio::test]
    async fn bound_generations_persist_floor_and_burn_range_on_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gens = SessionGenerations::new(dir.path());
        gens.bind("session-1").await;
        assert_eq!(gens.next(), Some(1));
        assert_eq!(gens.next(), Some(2));
        gens.idle().await;
        gens.close();

        let reopened = SessionGenerations::new(dir.path());
        reopened.bind("session-1").await;
        assert_eq!(reopened.next(), Some(RANGE + 1));
        reopened.close();
    }

    #[tokio::test]
    async fn contention_on_same_session_refuses_second_owner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = SessionGenerations::new(dir.path());
        first.bind("session-1").await;
        assert_eq!(first.next(), Some(1));

        let second = SessionGenerations::new(dir.path());
        second.bind("session-1").await;
        assert_eq!(second.next(), None);

        first.close();
    }
}
