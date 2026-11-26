//! Port of senpi `packages/coding-agent/src/core/trust-store-writers.ts`.
//!
//! Process-wide turn serialization and re-entrant lock tracking for `trust.json` writes so an async
//! writer and a synchronous reader in the same process never deadlock or wait out the file lock.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};
use std::thread::ThreadId;

static WRITE_QUEUES: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Default::default);
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
enum LockOwner {
    Task(tokio::task::Id),
    Thread(ThreadId),
}

fn current_owner() -> LockOwner {
    tokio::task::try_id().map_or_else(
        || LockOwner::Thread(std::thread::current().id()),
        LockOwner::Task,
    )
}

static HELD_PATHS: LazyLock<Mutex<HashMap<(String, LockOwner), usize>>> =
    LazyLock::new(Default::default);
tokio::task_local! {
    static TASK_LOCK_PATH: String;
}

pub struct TrustStoreLockGuard {
    path: String,
    owner: LockOwner,
}

impl Drop for TrustStoreLockGuard {
    fn drop(&mut self) {
        let mut paths = HELD_PATHS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = (self.path.clone(), self.owner);
        if let Some(count) = paths.get_mut(&key) {
            *count -= 1;
            if *count == 0 {
                paths.remove(&key);
            }
        }
    }
}

/// Runs `write` once every earlier write in this process to `path` has finished.
pub async fn in_trust_store_write_turn<T, F, Fut>(path: &str, write: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    let mutex = {
        let mut queues = WRITE_QUEUES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(queues.entry(path.to_owned()).or_default())
    };
    let _guard = mutex.lock().await;
    write().await
}

pub(crate) async fn in_trust_store_lock_scope<T>(path: &str, write: impl Future<Output = T>) -> T {
    TASK_LOCK_PATH.scope(path.to_owned(), write).await
}

/// Records that the current task or thread holds the store lock for `path` until the guard is dropped.
pub fn hold_trust_store_lock_in_realm(path: &str) -> TrustStoreLockGuard {
    let owner = current_owner();
    *HELD_PATHS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry((path.to_owned(), owner))
        .or_default() += 1;
    TrustStoreLockGuard {
        path: path.to_owned(),
        owner,
    }
}

/// Whether the current task or thread holds the store lock for `path`.
pub fn trust_store_lock_held_in_realm(path: &str) -> bool {
    if TASK_LOCK_PATH
        .try_with(|held| held == path)
        .unwrap_or(false)
    {
        return true;
    }
    HELD_PATHS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains_key(&(path.to_owned(), current_owner()))
}
