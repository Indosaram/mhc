//! Foreground dialog origin tracking (`runners/child-dialogs/foreground-origins.ts`, `#6189 F2d`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, PoisonError};

/// Tracks which children a parent turn is waiting on in the foreground. While entered, a child's
/// forwarded dialog targets the foreground origin; once the foreground wait exits (settled,
/// promoted, or aborted), background handlers take over.
#[derive(Debug, Default)]
pub struct ForegroundDialogOrigins {
    next_token: AtomicU64,
    active: Mutex<HashMap<String, u64>>,
}

impl ForegroundDialogOrigins {
    #[must_use]
    pub fn enter(&self, task_id: &str) -> Box<dyn FnOnce() + Send + '_> {
        let token = self.next_token.fetch_add(1, Ordering::AcqRel) + 1;
        let key = task_id.to_string();
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.clone(), token);
        Box::new(move || {
            let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
            if active.get(&key).copied() == Some(token) {
                active.remove(&key);
            }
        })
    }

    #[must_use]
    pub fn is_foreground(&self, task_id: &str) -> bool {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(task_id)
    }
}

static SHARED_ORIGINS: LazyLock<ForegroundDialogOrigins> =
    LazyLock::new(ForegroundDialogOrigins::default);

pub fn shared_foreground_dialog_origins() -> &'static ForegroundDialogOrigins {
    &SHARED_ORIGINS
}
