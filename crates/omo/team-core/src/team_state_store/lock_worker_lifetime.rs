//! Worker-lifetime fencing for lock records (`team-state-store/lock-worker-lifetime.ts`).
//!
//! Tracks lock records per worker incarnation so a live sibling worker inside the same OS process
//! is never reaped as stale, while a worker whose scope/thread has exited (or whose record was
//! explicitly marked released) is immediately reclaimable.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex, PoisonError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordStatus {
    Held,
    Released,
}

struct LifetimeState {
    main_incarnation: String,
    records: HashMap<String, (String, RecordStatus)>,
    exited: HashSet<String>,
}

impl LifetimeState {
    fn new() -> Self {
        Self {
            main_incarnation: uuid::Uuid::new_v4().to_string(),
            records: HashMap::new(),
            exited: HashSet::new(),
        }
    }
}

static STATE: LazyLock<Mutex<LifetimeState>> = LazyLock::new(|| Mutex::new(LifetimeState::new()));

/// Process-wide default incarnation id (kept for callers inspecting the host incarnation).
pub static LOCK_OWNER_INSTANCE_ID: LazyLock<String> = LazyLock::new(|| {
    STATE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .main_incarnation
        .clone()
});

thread_local! {
    static WORKER_INCARNATION: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Returns the active lock-owner incarnation id for the current thread (a scoped worker's own
/// incarnation when inside a [`LockWorkerScope`], or the process-wide incarnation otherwise).
#[must_use]
pub fn lock_owner_instance_id() -> String {
    WORKER_INCARNATION.with(|cell| {
        cell.borrow()
            .as_ref()
            .cloned()
            .unwrap_or_else(|| LOCK_OWNER_INSTANCE_ID.clone())
    })
}

/// RAII handle representing an observed worker thread lifetime. When dropped (or explicitly exited),
/// its incarnation is marked exited so any lock records it left behind become stale immediately.
pub struct LockWorkerScope {
    incarnation: String,
    previous: Option<String>,
    exited: bool,
}

impl LockWorkerScope {
    #[must_use]
    pub fn enter() -> Self {
        let incarnation = uuid::Uuid::new_v4().to_string();
        let previous =
            WORKER_INCARNATION.with(|cell| cell.borrow_mut().replace(incarnation.clone()));
        Self {
            incarnation,
            previous,
            exited: false,
        }
    }

    #[must_use]
    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }

    pub fn exit(mut self) {
        self.mark_exited();
    }

    fn mark_exited(&mut self) {
        if self.exited {
            return;
        }
        self.exited = true;
        STATE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .exited
            .insert(self.incarnation.clone());
        let prev = self.previous.take();
        WORKER_INCARNATION.with(|cell| {
            *cell.borrow_mut() = prev;
        });
    }
}

impl Drop for LockWorkerScope {
    fn drop(&mut self) {
        self.mark_exited();
    }
}

/// Marks a registered worker incarnation exited externally (e.g. if a host observes worker exit).
pub fn mark_lock_worker_exited(incarnation: &str) {
    STATE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .exited
        .insert(incarnation.to_owned());
}

pub fn mark_lock_record_held(content: &str) {
    let incarnation = lock_owner_instance_id();
    STATE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .records
        .insert(content.to_owned(), (incarnation, RecordStatus::Held));
}

pub fn mark_lock_record_released(content: &str) {
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, status)) = state.records.get_mut(content) {
        *status = RecordStatus::Released;
    }
}

pub fn forget_lock_record(content: &str) {
    STATE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .records
        .remove(content);
}

#[must_use]
pub fn is_worker_lock_released(incarnation: &str, record: &str) -> bool {
    let state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    if state.exited.contains(incarnation) {
        return true;
    }
    match state.records.get(record) {
        Some((owner, status)) if owner == incarnation => *status == RecordStatus::Released,
        // Unknown or unobserved incarnation: elapsed time never certifies worker death.
        _ => false,
    }
}
