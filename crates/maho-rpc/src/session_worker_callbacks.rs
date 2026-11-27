//! Port of senpi `packages/coding-agent/src/modes/rpc/session-worker-callbacks.ts`.
//!
//! Lifecycle hooks the owning registry installs on every worker it allocates.

use crate::session_worker_protocol::SessionWriteGrant;
use futures::future::BoxFuture;
use std::sync::Arc;

pub type ReserveHook = Arc<dyn Fn(&str) -> BoxFuture<'static, SessionWriteGrant> + Send + Sync>;
pub type ReconcileHook = Arc<dyn Fn(&[String]) -> BoxFuture<'static, ()> + Send + Sync>;
pub type ExitHook = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;
pub type FailureHook = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone)]
pub struct SessionWorkerCallbacks {
    pub reserve: ReserveHook,
    /// Every snapshot republishes which paths this worker still writes.
    pub reconcile: ReconcileHook,
    pub exit: ExitHook,
    pub failure: FailureHook,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[tokio::test]
    async fn callbacks_invoke_hooks() {
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let l1 = log.clone();
        let l2 = log.clone();
        let l3 = log.clone();
        let l4 = log.clone();
        let cb = SessionWorkerCallbacks {
            reserve: Arc::new(move |p| {
                let l1 = l1.clone();
                let p = p.to_owned();
                Box::pin(async move {
                    l1.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(format!("reserve:{p}"));
                    SessionWriteGrant::Granted
                })
            }),
            reconcile: Arc::new(move |paths| {
                let l2 = l2.clone();
                let count = paths.len();
                Box::pin(async move {
                    l2.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(format!("reconcile:{count}"));
                })
            }),
            exit: Arc::new(move || {
                let l3 = l3.clone();
                Box::pin(async move {
                    l3.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push("exit".into());
                })
            }),
            failure: Arc::new(move |err| {
                l4.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(format!("failure:{err}"));
            }),
        };
        assert_eq!((cb.reserve)("/a").await, SessionWriteGrant::Granted);
        (cb.reconcile)(&["/a".into()]).await;
        (cb.exit)().await;
        (cb.failure)("boom");
        assert_eq!(
            *log.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["reserve:/a", "reconcile:1", "exit", "failure:boom"]
        );
    }
}
