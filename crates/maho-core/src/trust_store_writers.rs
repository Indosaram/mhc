//! Port of senpi `packages/coding-agent/src/core/trust-store-writers.ts`.
//!
//! Process-wide turn serialization and re-entrant lock tracking for `trust.json` writes so an async
//! writer and a synchronous reader in the same process never deadlock or wait out the file lock.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};

static WRITE_QUEUES: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Default::default);
static HELD_PATHS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

pub struct TrustStoreLockGuard {
    path: String,
}

impl Drop for TrustStoreLockGuard {
    fn drop(&mut self) {
        HELD_PATHS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.path);
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

/// Records that a write in this process holds the store lock for `path` until the guard is dropped.
pub fn hold_trust_store_lock_in_realm(path: &str) -> TrustStoreLockGuard {
    HELD_PATHS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(path.to_owned());
    TrustStoreLockGuard {
        path: path.to_owned(),
    }
}

/// Whether a write in this process holds the store lock for `path` across awaits.
pub fn trust_store_lock_held_in_realm(path: &str) -> bool {
    HELD_PATHS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(path)
}
