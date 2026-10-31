//! Scoped-retry stop registry from `lifecycle/deferred-revival.ts`.
//!
//! Only `stopScopedRetries`/`resumeScopedRetries`/`disposeScopedRetries`/`retriesStopped` are
//! ported; `retryDeferredScopedChild` predates this sync and is still absent.

use std::collections::HashSet;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::context::LifecycleContext;

/// Sessions whose retries must stop: the engine shut that session down or was disposed. A retry
/// re-checks this after every wait, so it never revives a child under a session the user has left.
/// TS keys two WeakMaps by the context object; every clone of one resolved context shares this.
#[derive(Default)]
pub struct ScopedRetries {
    state: Mutex<StopState>,
    changed: Condvar,
}

#[derive(Default)]
struct StopState {
    stopped_sessions: HashSet<String>,
    disposed: bool,
}

impl ScopedRetries {
    fn state(&self) -> std::sync::MutexGuard<'_, StopState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The default retry wait: the backoff elapses, or dispose cuts it short.
    // PERF-DIVERGENCE(omo lifecycle/host-session.ts): upstream's `wait` is a timer the disposed
    // engine simply never observes again; a native retry thread would otherwise sleep out its whole
    // backoff after dispose, so the wait returns as soon as the lifecycle is disposed.
    pub fn wait(&self, ms: u64) {
        let deadline = Instant::now() + Duration::from_millis(ms);
        let mut state = self.state();
        while !state.disposed {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return;
            }
            state = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

pub fn stop_scoped_retries(context: &LifecycleContext, parent_session_id: &str) {
    context
        .scoped_retries
        .state()
        .stopped_sessions
        .insert(parent_session_id.to_string());
}

/// A session that is resumed again may retry its children again.
pub fn resume_scoped_retries(context: &LifecycleContext, parent_session_id: &str) {
    context
        .scoped_retries
        .state()
        .stopped_sessions
        .remove(parent_session_id);
}

pub fn dispose_scoped_retries(context: &LifecycleContext) {
    context.scoped_retries.state().disposed = true;
    context.scoped_retries.changed.notify_all();
}

pub fn retries_stopped(context: &LifecycleContext, parent_session_id: &str) -> bool {
    let state = context.scoped_retries.state();
    state.disposed || state.stopped_sessions.contains(parent_session_id)
}
