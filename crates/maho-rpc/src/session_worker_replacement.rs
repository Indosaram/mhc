//! Port of senpi `packages/coding-agent/src/modes/rpc/session-worker-replacement.ts`.
//!
//! Reserve the replacement identity before bootstrap publishes its first cut and returns the
//! worker's credit.

use crate::session_acceptance_lane::SessionAcceptanceLane;
use crate::session_worker_callbacks::SessionWorkerCallbacks;
use crate::session_worker_protocol::WorkerSnapshot;
use futures::future::BoxFuture;
use maho_core::engine_socket_flag::compact_updates_enabled;

/// Optional host-generation binder (`lane.generations.bind(sessionId)`).
pub type BindGeneration<'a> = &'a dyn Fn(&str) -> BoxFuture<'static, ()>;

pub async fn reconcile_worker_replacement(
    snapshot: Option<&WorkerSnapshot>,
    has_bootstrap: bool,
    lane: Option<&SessionAcceptanceLane>,
    bind_generation: Option<BindGeneration<'_>>,
    callbacks: &SessionWorkerCallbacks,
) {
    let Some(snapshot) = snapshot else {
        return;
    };
    if has_bootstrap && compact_updates_enabled() {
        let _ = lane;
        if let Some(session_id) = snapshot
            .state
            .get("sessionId")
            .and_then(serde_json::Value::as_str)
            && let Some(bind) = bind_generation
        {
            bind(session_id).await;
        }
    }
    (callbacks.reconcile)(&snapshot.live_session_paths).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_worker_protocol::SessionWriteGrant;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn reconcile_runs_only_when_snapshot_is_present() {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let c1 = calls.clone();
        let cb = SessionWorkerCallbacks {
            reserve: Arc::new(|_| Box::pin(async { SessionWriteGrant::Granted })),
            reconcile: Arc::new(move |paths| {
                let c1 = c1.clone();
                let joined = paths.join(",");
                Box::pin(async move {
                    c1.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(joined);
                })
            }),
            exit: Arc::new(|| Box::pin(async {})),
            failure: Arc::new(|_| {}),
        };
        reconcile_worker_replacement(None, false, None, None, &cb).await;
        assert!(
            calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );

        let snap = WorkerSnapshot {
            state: serde_json::json!({ "sessionId": "s1" }),
            session_path: None,
            live_session_paths: vec!["/a".into(), "/b".into()],
            busy: false,
            handoff_busy: None,
            streaming: false,
        };
        reconcile_worker_replacement(Some(&snap), false, None, None, &cb).await;
        assert_eq!(
            *calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["/a,/b"]
        );
    }
}
