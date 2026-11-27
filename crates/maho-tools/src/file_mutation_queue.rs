//! Re-exported from `maho-machine-ops` (`senpi#5592`), with `ToolError` convenience adapters.

use crate::definition::ToolError;
use std::future::Future;
use std::path::Path;

pub use maho_machine_ops::files::mutation_queue::{
    CancelSignal, FileLockedByStuckModuleError, FileMutationLock, FileMutationLockError,
    acquire_file_mutation_lock, with_file_mutation_queues,
};

impl From<FileMutationLockError> for ToolError {
    fn from(error: FileMutationLockError) -> Self {
        match error {
            FileMutationLockError::Op(op_err) => {
                crate::fs_error_code::tool_error_from_machine(op_err)
            }
            other => ToolError::Message(other.to_string()),
        }
    }
}

pub async fn lock_file_mutation(path: &Path) -> Result<FileMutationLock, ToolError> {
    Ok(acquire_file_mutation_lock(path, None).await?)
}

/// Serialize file mutation operations targeting the same file; other files still run in parallel.
pub async fn with_file_mutation_queue<T, F, Fut>(path: &Path, f: F) -> Result<T, ToolError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    let _lock = lock_file_mutation(path).await?;
    Ok(f().await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::watch;
    use tokio::time::timeout;

    const BOUND: Duration = Duration::from_secs(5);
    const CANCELLED_MESSAGE: &str = "file mutation lock wait cancelled";

    #[tokio::test]
    async fn waiters_are_serialized_and_release_passes_on() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.txt");
        let first = acquire_file_mutation_lock(&path, None)
            .await
            .expect("first");
        let second = tokio::spawn({
            let path = path.clone();
            async move { acquire_file_mutation_lock(&path, None).await.map(|_| ()) }
        });
        first.release();
        timeout(BOUND, second)
            .await
            .expect("bounded")
            .expect("join")
            .expect("second acquired");
    }

    #[tokio::test]
    async fn cancelled_waiter_does_not_hold_slot_and_later_waiter_proceeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.txt");
        let first = acquire_file_mutation_lock(&path, None)
            .await
            .expect("first");
        let (tx, rx) = watch::channel(None);
        let cancelled_wait = tokio::spawn({
            let path = path.clone();
            async move {
                acquire_file_mutation_lock(&path, Some(rx))
                    .await
                    .map(|_| ())
            }
        });
        let third = tokio::spawn({
            let path = path.clone();
            async move { acquire_file_mutation_lock(&path, None).await.map(|_| ()) }
        });
        tx.send_replace(Some(String::new()));
        let error = timeout(BOUND, cancelled_wait)
            .await
            .expect("bounded")
            .expect("join")
            .expect_err("cancelled");
        assert_eq!(error.to_string(), CANCELLED_MESSAGE);
        first.release();
        timeout(BOUND, third)
            .await
            .expect("bounded")
            .expect("join")
            .expect("third acquired");
    }

    #[tokio::test]
    async fn stuck_holder_fails_current_and_future_waiters() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.txt");
        let holder = acquire_file_mutation_lock(&path, None)
            .await
            .expect("holder");
        let waiting = tokio::spawn({
            let path = path.clone();
            async move { acquire_file_mutation_lock(&path, None).await.map(|_| ()) }
        });
        holder.mark_stuck("mod-x");
        let error = timeout(BOUND, waiting)
            .await
            .expect("bounded")
            .expect("join")
            .expect_err("stuck");
        let FileMutationLockError::Stuck(stuck) = &error else {
            panic!("expected stuck, got {error}")
        };
        assert_eq!(stuck.holder, "mod-x");
        assert_eq!(stuck.code(), "file_locked_by_stuck_module");
        let later = timeout(BOUND, acquire_file_mutation_lock(&path, None))
            .await
            .expect("bounded");
        assert!(matches!(later, Err(FileMutationLockError::Stuck(_))));
        holder.release();
    }
}
