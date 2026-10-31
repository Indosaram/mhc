//! Port of senpi packages/coding-agent/src/core/credential-pool/state-file.ts.
//!
//! The pool state file's lock, lock-free read, and atomic replace. Lock acquisition is the workspace
//! lockfile policy (`acquire_lock_async`), which already queues and retries within the budget.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::lockfile_policy::{CredentialStoreBusyError, LockGuard, acquire_lock_async};

/// Delays between retries of a replace that Windows refused transiently (antivirus, the indexer, a
/// lock-free reader holding the file open for a moment).
const RENAME_RETRY_DELAYS_MS: [u64; 5] = [10, 20, 40, 80, 160];

/// Waits for the file lock within `budget_ms`, then fails naming the lock and the wait.
pub async fn acquire_state_file_lock(
    path: &str,
    budget_ms: u64,
) -> Result<LockGuard, CredentialStoreBusyError> {
    acquire_lock_async(path, budget_ms).await
}

/// The file's content, or `None` when it does not exist yet.
pub fn read_state_file(path: &str) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[derive(Debug)]
pub enum StateFileWriteError {
    Io(std::io::Error),
    /// A transient refusal outlasted the retries; reads as a busy store.
    Busy(CredentialStoreBusyError),
}

impl std::fmt::Display for StateFileWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => std::fmt::Display::fmt(error, f),
            Self::Busy(error) => f.write_str(&error.message()),
        }
    }
}

impl std::error::Error for StateFileWriteError {}

fn is_transient_rename_error(error: &std::io::Error) -> bool {
    // EPERM, EBUSY, EACCES: Rust maps EPERM and EACCES to PermissionDenied and EBUSY to ResourceBusy.
    matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ResourceBusy
    )
}

/// Renames over the target, retrying a transiently refused replace; a refusal that outlasts the retries
/// reads as a busy store.
async fn replace_file(temporary: &Path, path: &str) -> Result<(), StateFileWriteError> {
    let started = Instant::now();
    let delays = RENAME_RETRY_DELAYS_MS
        .iter()
        .map(Some)
        .chain(std::iter::once(None));
    for delay_ms in delays {
        match std::fs::rename(temporary, path) {
            Ok(()) => return Ok(()),
            Err(error) if !is_transient_rename_error(&error) => {
                return Err(StateFileWriteError::Io(error));
            }
            Err(error) => match delay_ms {
                None => {
                    let waited = started.elapsed().as_millis() as u64;
                    let busy = CredentialStoreBusyError::new(format!("{path}: {error}"), waited);
                    return Err(StateFileWriteError::Busy(busy));
                }
                Some(delay_ms) => tokio::time::sleep(Duration::from_millis(*delay_ms)).await,
            },
        }
    }
    Ok(())
}

/// Stages the document beside the file and renames it over the target, so a reader that takes no lock
/// always sees one complete document. Writers stay serialized by the lock the caller holds, so the
/// pid-suffixed temp cannot collide.
pub async fn write_state_file_atomic(path: &str, content: &str) -> Result<(), StateFileWriteError> {
    let temporary = format!("{path}.{}.tmp", std::process::id());
    let temporary = Path::new(&temporary);
    let result = async {
        std::fs::write(temporary, content).map_err(StateFileWriteError::Io)?;
        set_mode(temporary, path).map_err(StateFileWriteError::Io)?;
        replace_file(temporary, path).await
    }
    .await;
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

/// New files are owner-only (0o600); an existing file keeps its mode.
#[cfg(unix)]
fn set_mode(temporary: &Path, path: &str) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = match std::fs::metadata(path) {
        Ok(metadata) => metadata.permissions().mode() & 0o777,
        Err(_) => 0o600,
    };
    std::fs::set_permissions(temporary, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_temporary: &Path, _path: &str) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_missing_file_as_absent() {
        let dir = tempfile::tempdir().ok();
        let path = dir
            .as_ref()
            .map(|dir| dir.path().join("missing.json"))
            .unwrap_or_default();
        assert!(matches!(read_state_file(&path.to_string_lossy()), Ok(None)));
    }

    #[tokio::test]
    async fn replaces_the_file_atomically_and_leaves_no_temp_behind() {
        let Some(dir) = tempfile::tempdir().ok() else {
            panic!("tempdir")
        };
        let path = dir.path().join("state.json").to_string_lossy().into_owned();
        assert!(write_state_file_atomic(&path, "{\"a\":1}").await.is_ok());
        assert!(write_state_file_atomic(&path, "{\"a\":2}").await.is_ok());
        assert_eq!(
            read_state_file(&path).ok().flatten().as_deref(),
            Some("{\"a\":2}")
        );
        let leftovers = std::fs::read_dir(dir.path())
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(leftovers, 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .map(|m| m.permissions().mode() & 0o777)
                .unwrap_or(0);
            assert_eq!(mode, 0o600);
        }
    }
}
