//! Port of senpi packages/coding-agent/src/core/session-work-barrier.ts.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Notify;

const SETTLE_ROUNDS_BEFORE_YIELD: usize = 16;

/// Tracks deferred session work: a begin() guard keeps the barrier active until the last guard is
/// dropped, and wait_for_settled polls the event queue until both the queue and the work settle.
#[derive(Debug, Default)]
pub struct SessionWorkBarrier {
    active_work_depth: AtomicUsize,
    notify: Arc<Notify>,
}

#[derive(Debug)]
pub struct SessionWorkGuard {
    barrier: Arc<SessionWorkBarrier>,
    finished: bool,
}

impl SessionWorkBarrier {
    pub fn new() -> Self {
        Self {
            active_work_depth: AtomicUsize::new(0),
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn has_active_work(&self) -> bool {
        self.active_work_depth.load(Ordering::SeqCst) > 0
    }

    pub fn begin(self: &Arc<Self>) -> SessionWorkGuard {
        self.active_work_depth.fetch_add(1, Ordering::SeqCst);
        SessionWorkGuard {
            barrier: Arc::clone(self),
            finished: false,
        }
    }

    /// Waits until the event queue stops moving and no work is active. The first rounds re-sample
    /// immediately so a continuation scheduled during the turn is still observed as pending; once
    /// the queue proves it is not converging, control yields to the runtime so a session that
    /// re-chains already-resolved work cannot pin a core.
    pub async fn wait_for_settled<F, Fut>(&self, get_event_queue: F)
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let mut round = 0usize;
        loop {
            let notified = self.notify.notified();
            let has_work = self.has_active_work();
            get_event_queue().await;
            if has_work {
                notified.await;
            }
            if !self.has_active_work() {
                return;
            }
            if round >= SETTLE_ROUNDS_BEFORE_YIELD {
                tokio::task::yield_now().await;
            }
            round += 1;
        }
    }

    fn finish(&self) {
        let previous = self.active_work_depth.fetch_sub(1, Ordering::SeqCst);
        if previous <= 1 {
            self.active_work_depth.store(0, Ordering::SeqCst);
            self.notify.notify_waiters();
        }
    }
}

impl SessionWorkGuard {
    pub fn finish(mut self) {
        self.finished = true;
        self.barrier.finish();
    }
}

impl Drop for SessionWorkGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.barrier.finish();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guard_marks_work_active_until_dropped() {
        let barrier = Arc::new(SessionWorkBarrier::new());
        assert!(!barrier.has_active_work());
        let guard = barrier.begin();
        assert!(barrier.has_active_work());
        drop(guard);
        assert!(!barrier.has_active_work());
    }

    #[test]
    fn nested_guards_keep_the_barrier_active_until_the_last_drops() {
        let barrier = Arc::new(SessionWorkBarrier::new());
        let first = barrier.begin();
        let second = barrier.begin();
        drop(first);
        assert!(barrier.has_active_work());
        drop(second);
        assert!(!barrier.has_active_work());
    }

    #[test]
    fn finishing_twice_is_idempotent() {
        let barrier = Arc::new(SessionWorkBarrier::new());
        let guard = barrier.begin();
        guard.finish();
        assert!(!barrier.has_active_work());
    }

    #[tokio::test]
    async fn waiting_returns_once_no_work_remains() {
        let barrier = Arc::new(SessionWorkBarrier::new());
        let guard = barrier.begin();
        let barrier_clone = Arc::clone(&barrier);
        let release = tokio::spawn(async move {
            tokio::task::yield_now().await;
            drop(guard);
        });
        barrier_clone.wait_for_settled(|| async {}).await;
        release.await.expect("release");
        assert!(!barrier.has_active_work());
    }

    #[test]
    fn tool_barrier_spec_validation() {
        assert!(tool_barrier_spec_error(&serde_json::json!({})).is_some());
        assert!(
            tool_barrier_spec_error(&serde_json::json!({"id": "", "deadlineMs": 1000})).is_some()
        );
        assert!(
            tool_barrier_spec_error(&serde_json::json!({"id": "valid", "deadlineMs": 0})).is_some()
        );
        assert!(
            tool_barrier_spec_error(&serde_json::json!({"id": "valid", "deadlineMs": 700000}))
                .is_some()
        );
        assert!(
            tool_barrier_spec_error(&serde_json::json!({"id": "valid", "deadlineMs": 1000}))
                .is_none()
        );
    }

    #[tokio::test]
    async fn tool_barrier_arms_and_releases() {
        let expired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let expired_clone = Arc::clone(&expired);
        let barrier = ToolBarrier::new(move |_| {
            expired_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        barrier.arm(ToolBarrierSpec {
            id: "b-1".into(),
            deadline_ms: 10_000,
        });

        assert!(!barrier.release("wrong-id"));
        assert!(barrier.release("b-1"));
        assert!(!expired.load(std::sync::atomic::Ordering::SeqCst));
    }
}

// ---------------------------------------------------------------------------
// Tool barrier (senpi #3717)
// ---------------------------------------------------------------------------

/// What a tool can do to the workspace. Tools that do not declare it are treated as "write".
pub use maho_tools::definition::ToolWorkspaceEffect;

pub const MAX_TOOL_BARRIER_ID_LENGTH: usize = 256;
pub const MAX_TOOL_BARRIER_DEADLINE_MS: u64 = 600_000;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolBarrierSpec {
    pub id: String,
    pub deadline_ms: u64,
}

pub fn tool_barrier_spec_error(value: &serde_json::Value) -> Option<String> {
    let obj = match value.as_object() {
        Some(obj) => obj,
        None => return Some("toolBarrier must be an object { id, deadlineMs }.".into()),
    };
    let id = match obj.get("id").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() && s.len() <= MAX_TOOL_BARRIER_ID_LENGTH => s,
        _ => {
            return Some(format!(
                "toolBarrier.id must be a non-empty string of at most {MAX_TOOL_BARRIER_ID_LENGTH} characters."
            ));
        }
    };
    let _ = id;
    let deadline_ms = match obj.get("deadlineMs").and_then(|v| v.as_u64()) {
        Some(n) if (1..=MAX_TOOL_BARRIER_DEADLINE_MS).contains(&n) => n,
        _ => {
            return Some(format!(
                "toolBarrier.deadlineMs must be a whole number of milliseconds in 1..{MAX_TOOL_BARRIER_DEADLINE_MS}."
            ));
        }
    };
    let _ = deadline_ms;
    None
}

struct ArmedBarrier {
    spec: ToolBarrierSpec,
    notify: Arc<tokio::sync::Notify>,
    timer_handle: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
pub struct ToolBarrier {
    armed: Arc<std::sync::Mutex<Option<ArmedBarrier>>>,
    on_expired: Arc<dyn Fn(ToolBarrierSpec) + Send + Sync>,
}

impl ToolBarrier {
    pub fn new<F: Fn(ToolBarrierSpec) + Send + Sync + 'static>(on_expired: F) -> Self {
        Self {
            armed: Arc::new(std::sync::Mutex::new(None)),
            on_expired: Arc::new(on_expired),
        }
    }

    pub fn arm(&self, spec: ToolBarrierSpec) {
        let mut guard = self
            .armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(previous) = guard.take() {
            previous.timer_handle.abort();
            previous.notify.notify_waiters();
        }

        let notify = Arc::new(tokio::sync::Notify::new());
        let notify_clone = Arc::clone(&notify);
        let armed_clone = Arc::clone(&self.armed);
        let on_expired = Arc::clone(&self.on_expired);
        let spec_clone = spec.clone();

        let timer_handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(spec_clone.deadline_ms)).await;
            let expired = {
                let mut guard = armed_clone
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if guard.as_ref().is_some_and(|a| a.spec.id == spec_clone.id) {
                    guard.take()
                } else {
                    None
                }
            };
            if let Some(armed) = expired {
                notify_clone.notify_waiters();
                on_expired(armed.spec);
            }
        });

        *guard = Some(ArmedBarrier {
            spec,
            notify,
            timer_handle,
        });
    }

    pub fn id(&self) -> Option<String> {
        self.armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|a| a.spec.id.clone())
    }

    pub fn release(&self, id: &str) -> bool {
        let mut guard = self
            .armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.as_ref().is_some_and(|a| a.spec.id == id)
            && let Some(armed) = guard.take() {
                armed.timer_handle.abort();
                armed.notify.notify_waiters();
                return true;
            }
        false
    }

    pub fn clear(&self) {
        let mut guard = self
            .armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(armed) = guard.take() {
            armed.timer_handle.abort();
            armed.notify.notify_waiters();
        }
    }

    pub async fn wait(
        &self,
        signal: Option<&maho_ai::utils::abort::AbortSignal>,
    ) -> Option<String> {
        // Register while the armed state is locked, before release can notify.
        let (id, notified) = {
            let guard = self
                .armed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let armed = guard.as_ref()?;
            let mut notified = Box::pin(Arc::clone(&armed.notify).notified_owned());
            notified.as_mut().enable();
            (armed.spec.id.clone(), notified)
        };

        if let Some(sig) = signal {
            if sig.aborted() {
                return Some(id);
            }
            tokio::select! {
                _ = notified => Some(id),
                _ = sig.cancelled() => Some(id),
            }
        } else {
            notified.await;
            Some(id)
        }
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    #[tokio::test]
    async fn aborted_barrier_wait_returns_without_release() {
        let barrier = ToolBarrier::new(|_| {});
        barrier.arm(ToolBarrierSpec {
            id: "blocked".into(),
            deadline_ms: 60_000,
        });
        let signal = maho_ai::utils::abort::AbortController::new();
        let wait_signal = signal.signal();
        signal.abort(None);
        assert_eq!(
            barrier.wait(Some(&wait_signal)).await.as_deref(),
            Some("blocked")
        );
        assert!(barrier.release("blocked"));
    }
}
