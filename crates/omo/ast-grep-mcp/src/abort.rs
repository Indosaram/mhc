//! A cloneable abort signal, standing in for the reference implementation's `AbortSignal`.

use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Duration;
use std::time::Instant;

/// Shared cancellation flag. Every clone observes the same state; the first `abort` wins.
#[derive(Clone, Default)]
pub struct AbortSignal {
    inner: Arc<(Mutex<Option<String>>, Condvar)>,
}

impl AbortSignal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks the signal aborted with `reason` and wakes every waiter. Later calls are no-ops.
    pub fn abort(&self, reason: impl Into<String>) {
        let (lock, condvar) = &*self.inner;
        let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
        if state.is_none() {
            *state = Some(reason.into());
            condvar.notify_all();
        }
    }

    pub fn is_aborted(&self) -> bool {
        self.reason().is_some()
    }

    pub fn reason(&self) -> Option<String> {
        let (lock, _) = &*self.inner;
        lock.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Blocks until the signal is aborted or `timeout` elapses; returns the abort reason if any.
    pub fn wait_for_abort(&self, timeout: Duration) -> Option<String> {
        let (lock, condvar) = &*self.inner;
        let deadline = Instant::now() + timeout;
        let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
        while state.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            state = condvar
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        state.clone()
    }
}

impl std::fmt::Debug for AbortSignal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AbortSignal")
            .field("reason", &self.reason())
            .finish()
    }
}
