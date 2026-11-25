//! The one terminal finalizer of a startup attempt (senpi#4602, A-amended section 6). Every failure
//! path (timeout, explicit failure, protocol fault, registration error) settles through it exactly once:
//! it latches the original cause, runs the owned cleanup inside its nonrenewable allowance, and reports
//! `cleanup=incomplete` when that cleanup did not finish in time or failed. The original diagnostic
//! always comes first; a cleanup failure is appended after it, never instead of it.

use crate::host_stalled_evidence::child_stalled_stop_max_ms;
use crate::host_startup_policy::{StartupClock, startup_envelope, startup_timeout_detail};
use crate::host_startup_run::{StartupFailure, monotonic_startup_clock};
use futures::future::{BoxFuture, FutureExt, Shared};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupStatus {
    Completed,
    Incomplete,
}

impl CleanupStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Incomplete => "incomplete",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupFinalReport {
    pub outcome: StartupFailure,
    pub cleanup: CleanupStatus,
    pub detail: String,
}

pub type CleanupAction = Box<dyn FnOnce() -> BoxFuture<'static, Result<(), String>> + Send>;

pub struct StartupFinalizer {
    clock: Arc<dyn StartupClock>,
    cleanup_budget_ms: u64,
    cleanup: Mutex<Option<CleanupAction>>,
    settled: Mutex<Option<Shared<BoxFuture<'static, StartupFinalReport>>>>,
}

impl StartupFinalizer {
    pub fn new(
        clock: Arc<dyn StartupClock>,
        cleanup_budget_ms: u64,
        cleanup: impl FnOnce() -> BoxFuture<'static, Result<(), String>> + Send + 'static,
    ) -> Self {
        Self {
            clock,
            cleanup_budget_ms,
            cleanup: Mutex::new(Some(Box::new(cleanup))),
            settled: Mutex::new(None),
        }
    }

    pub fn finalize(
        &self,
        outcome: StartupFailure,
    ) -> Shared<BoxFuture<'static, StartupFinalReport>> {
        let mut settled_guard = self.settled.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = settled_guard.as_ref() {
            return existing.clone();
        }
        let clock = self.clock.clone();
        let cleanup_budget_ms = self.cleanup_budget_ms;
        let cleanup_fn = self
            .cleanup
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let fut = async move {
            let (exp_tx, exp_rx) = oneshot::channel::<()>();
            let timer = clock.schedule_at(
                clock.now_ms().saturating_add(cleanup_budget_ms),
                Box::new(move || {
                    let _ = exp_tx.send(());
                }),
            );
            let run = match cleanup_fn {
                Some(action) => action(),
                None => Box::pin(async { Ok(()) }),
            };
            enum CleanupOutcome {
                Done(Result<(), String>),
                Expired,
            }
            let result = tokio::select! {
                res = run => CleanupOutcome::Done(res),
                _ = exp_rx => CleanupOutcome::Expired,
            };
            timer.cancel();

            let (cleanup, appended) = match result {
                CleanupOutcome::Done(Ok(())) => (CleanupStatus::Completed, String::new()),
                CleanupOutcome::Done(Err(err)) => (
                    CleanupStatus::Incomplete,
                    format!("\ncleanup failed: {err}"),
                ),
                CleanupOutcome::Expired => (
                    CleanupStatus::Incomplete,
                    format!("\ncleanup did not finish within {cleanup_budget_ms}ms"),
                ),
            };
            let original = match &outcome {
                StartupFailure::Timeout(timeout) => {
                    startup_timeout_detail(timeout, cleanup.as_str())
                }
                StartupFailure::Failed { cause, .. } => cause.clone(),
            };
            StartupFinalReport {
                outcome,
                cleanup,
                detail: format!("{original}{appended}"),
            }
        }
        .boxed()
        .shared();
        *settled_guard = Some(fut.clone());
        fut
    }
}

/// One failed `startHost` attempt, settled once: owned cleanup inside the failed-start allowance C
/// at the resolved stall setting.
pub async fn finalize_failed_start(
    outcome: StartupFailure,
    cleanup: impl FnOnce() -> BoxFuture<'static, Result<(), String>> + Send + 'static,
) -> StartupFinalReport {
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let budget_ms = startup_envelope(Some(child_stalled_stop_max_ms(&env))).cleanup;
    let finalizer = StartupFinalizer::new(monotonic_startup_clock(), budget_ms, cleanup);
    finalizer.finalize(outcome).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_startup_policy::{StartupPhase, StartupTimeout, TimeoutLimit};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn finalizer_runs_once_and_latches_original_outcome() {
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = runs.clone();
        let finalizer = StartupFinalizer::new(monotonic_startup_clock(), 5_000, move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err("rm failed".to_owned()) })
        });
        let first = finalizer
            .finalize(StartupFailure::Timeout(StartupTimeout {
                phase: StartupPhase::HostRuntime,
                limit: TimeoutLimit::Phase,
                phase_budget_ms: 60_000,
                elapsed_ms: 60_000,
            }))
            .await;
        let second = finalizer
            .finalize(StartupFailure::Failed {
                phase: StartupPhase::PublicVerified,
                cause: "later cause".into(),
                elapsed_ms: 1,
            })
            .await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(first, second);
        assert_eq!(first.cleanup, CleanupStatus::Incomplete);
        assert!(
            first
                .detail
                .starts_with("RPC host startup timed out: phase=host_runtime")
        );
        assert!(first.detail.contains("cleanup=incomplete"));
        assert!(first.detail.ends_with("\ncleanup failed: rm failed"));
    }

    #[tokio::test(start_paused = true)]
    async fn finalizer_bounds_hung_cleanup_by_budget() {
        let finalizer = StartupFinalizer::new(monotonic_startup_clock(), 100, || {
            Box::pin(std::future::pending())
        });
        let report = finalizer
            .finalize(StartupFailure::Failed {
                phase: StartupPhase::HostBootstrap,
                cause: "child died".into(),
                elapsed_ms: 50,
            })
            .await;
        assert_eq!(report.cleanup, CleanupStatus::Incomplete);
        assert_eq!(
            report.detail,
            "child died\ncleanup did not finish within 100ms"
        );
    }
}
