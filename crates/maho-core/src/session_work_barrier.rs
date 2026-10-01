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
        Self { active_work_depth: AtomicUsize::new(0), notify: Arc::new(Notify::new()) }
    }

    pub fn has_active_work(&self) -> bool {
        self.active_work_depth.load(Ordering::SeqCst) > 0
    }

    pub fn begin(self: &Arc<Self>) -> SessionWorkGuard {
        self.active_work_depth.fetch_add(1, Ordering::SeqCst);
        SessionWorkGuard { barrier: Arc::clone(self), finished: false }
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
}
