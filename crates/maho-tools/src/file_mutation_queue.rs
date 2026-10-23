use crate::{definition::ToolError, filesystem_policy::canonicalize_filesystem_path};
use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    sync::{
        Mutex as StdMutex, OnceLock, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{Mutex, watch};

/// Cancellation signal for lock waiters: becomes `Some(reason)` when the wait is cancelled.
pub type CancelSignal = watch::Receiver<Option<String>>;

const CANCELLED_MESSAGE: &str = "file mutation lock wait cancelled";

/// A file whose slot is held by a module that is stuck (past its deadline and grace, still running in this process
/// where nothing can stop it). The slot stays held, so every waiter fails with this instead of waiting forever.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "file_locked_by_stuck_module: {path} is locked by {holder}, which is stuck past its deadline; it is released when that module finishes or the session is closed"
)]
pub struct FileLockedByStuckModuleError {
    pub path: String,
    pub holder: String,
}

impl FileLockedByStuckModuleError {
    pub const CODE: &'static str = "file_locked_by_stuck_module";
    pub fn code(&self) -> &'static str {
        Self::CODE
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FileMutationLockError {
    #[error(transparent)]
    Stuck(#[from] FileLockedByStuckModuleError),
    #[error("{0}")]
    Cancelled(String),
    #[error(transparent)]
    Tool(#[from] ToolError),
}

impl From<FileMutationLockError> for ToolError {
    fn from(error: FileMutationLockError) -> Self {
        match error {
            FileMutationLockError::Tool(error) => error,
            other => ToolError::Message(other.to_string()),
        }
    }
}

type StuckState = Option<FileLockedByStuckModuleError>;

struct Entry {
    tail: watch::Receiver<bool>,
    tail_id: u64,
    stuck: watch::Sender<StuckState>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static REGISTRY: OnceLock<StdMutex<HashMap<PathBuf, Entry>>> = OnceLock::new();
static REGISTRATION: OnceLock<Mutex<()>> = OnceLock::new();

fn registry() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Entry>> {
    REGISTRY
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// A queued position in a file's mutation queue. Dropping it passes the turn on once its predecessor finished.
struct Slot {
    key: PathBuf,
    id: u64,
    prev: Option<watch::Receiver<bool>>,
    done: Option<watch::Sender<bool>>,
    held: bool,
}

async fn wait_done(prev: Option<watch::Receiver<bool>>) {
    if let Some(mut prev) = prev {
        // A dropped sender means the predecessor is gone, which also ends its turn.
        let _ = prev.wait_for(|done| *done).await;
    }
}

fn release_slot(key: &Path, id: u64, done: &watch::Sender<bool>, was_held: bool) {
    {
        let mut queues = registry();
        if let Some(entry) = queues.get(key) {
            if was_held {
                entry.stuck.send_replace(None);
            }
            if entry.tail_id == id {
                queues.remove(key);
            }
        }
    }
    done.send_replace(true);
}

impl Drop for Slot {
    fn drop(&mut self) {
        let Some(done) = self.done.take() else { return };
        let (key, id, held) = (self.key.clone(), self.id, self.held);
        if held {
            release_slot(&key, id, &done, true);
            return;
        }
        let prev = self.prev.take();
        match tokio::runtime::Handle::try_current() {
            // Never held: pass the queued slot on once the predecessor finishes, so the next waiter is not stranded.
            Ok(handle) => {
                handle.spawn(async move {
                    wait_done(prev).await;
                    release_slot(&key, id, &done, false);
                });
            }
            Err(_) => release_slot(&key, id, &done, false),
        }
    }
}

async fn register_slot(path: &Path) -> Result<Slot, ToolError> {
    let _order = REGISTRATION.get_or_init(|| Mutex::new(())).lock().await;
    let key = crate::bounded_realpath::fold_path_for_case_insensitive_filesystem(
        canonicalize_filesystem_path(path).await?,
    );
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let (done, tail) = watch::channel(false);
    let mut queues = registry();
    let prev = match queues.get_mut(&key) {
        Some(entry) => {
            let prev = std::mem::replace(&mut entry.tail, tail);
            entry.tail_id = id;
            Some(prev)
        }
        None => {
            queues.insert(
                key.clone(),
                Entry {
                    tail,
                    tail_id: id,
                    stuck: watch::channel(None).0,
                },
            );
            None
        }
    };
    Ok(Slot {
        key,
        id,
        prev,
        done: Some(done),
        held: false,
    })
}

async fn cancelled(cancel: Option<CancelSignal>) -> String {
    if let Some(mut cancel) = cancel {
        let reason = cancel
            .wait_for(|reason| reason.is_some())
            .await
            .ok()
            .map(|value| (*value).clone());
        if let Some(reason) = reason {
            return reason.unwrap_or_default();
        }
    }
    std::future::pending().await
}

/// Waits for the slot's turn; a waiter whose signal fires, or whose file's holder is (or becomes) stuck, fails
/// without ever holding the slot.
async fn wait_turn(
    slot: &mut Slot,
    cancel: Option<CancelSignal>,
) -> Result<(), FileMutationLockError> {
    let mut stuck = registry()
        .get(&slot.key)
        .map(|entry| entry.stuck.subscribe());
    let stuck_wait = async {
        let found = match stuck.as_mut() {
            Some(rx) => rx
                .wait_for(|state| state.is_some())
                .await
                .ok()
                .map(|state| (*state).clone()),
            None => None,
        };
        match found {
            Some(state) => state,
            None => std::future::pending().await,
        }
    };
    let outcome = tokio::select! {
        biased;
        () = wait_done(slot.prev.clone()) => Ok(()),
        error = stuck_wait => match error {
            Some(error) => Err(FileMutationLockError::Stuck(error)),
            None => std::future::pending().await,
        },
        reason = cancelled(cancel) => Err(FileMutationLockError::Cancelled(if reason.is_empty() { CANCELLED_MESSAGE.to_owned() } else { reason })),
    };
    if outcome.is_ok() {
        slot.held = true;
    }
    outcome
}

/// The holder's side of a slot. Releases on drop (once); [`FileMutationLock::mark_stuck`] keeps the slot held and
/// fails every current and future waiter on the key.
pub struct FileMutationLock {
    slot: Slot,
    path: String,
}

impl FileMutationLock {
    pub fn release(self) {}

    pub fn mark_stuck(&self, holder: &str) {
        if !self.slot.held {
            return;
        }
        let error = FileLockedByStuckModuleError {
            path: self.path.clone(),
            holder: holder.to_owned(),
        };
        if let Some(entry) = registry().get(&self.slot.key) {
            entry.stuck.send_replace(Some(error));
        }
    }
}

/// Takes the mutation slot of `path` once every earlier mutation of that file has finished. A waiter whose `cancel`
/// signal fires rejects without holding the slot; the slot it queued passes on after its predecessor.
pub async fn acquire_file_mutation_lock(
    path: &Path,
    cancel: Option<CancelSignal>,
) -> Result<FileMutationLock, FileMutationLockError> {
    let mut slot = register_slot(path).await?;
    wait_turn(&mut slot, cancel).await?;
    Ok(FileMutationLock {
        slot,
        path: path.to_string_lossy().into_owned(),
    })
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
    use tokio::time::timeout;

    const BOUND: Duration = Duration::from_secs(5);

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
