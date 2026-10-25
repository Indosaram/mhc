//! Port of senpi `experimental/session-worker-ownership.ts`.
use std::{future::Future, pin::Pin, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Duration};
use super::server::{FileLockLostError, bounded_report};

pub const OWNERSHIP_LOST_REPORT_TIMEOUT_MS: u64 = 1_000;
pub type ReportFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

pub struct OwnershipLossStop {
    pub report: Arc<dyn Fn(String) -> ReportFuture + Send + Sync>,
    pub exit: Arc<dyn Fn(i32) + Send + Sync>,
    pub report_timeout: Duration,
}
impl OwnershipLossStop {
    pub fn new(report: impl Fn(String) -> ReportFuture + Send + Sync + 'static, exit: impl Fn(i32) + Send + Sync + 'static) -> Self {
        Self { report: Arc::new(report), exit: Arc::new(exit), report_timeout: Duration::from_millis(OWNERSHIP_LOST_REPORT_TIMEOUT_MS) }
    }
}

/// There is deliberately no harness/close callback here: closing can flush entries into the new
/// owner's JSONL. A cloneable, once-only callback can be registered before opening any storage.
pub fn stop_worker_on_lost_ownership(stop: OwnershipLossStop) -> Arc<dyn Fn(FileLockLostError) + Send + Sync> {
    let stopping = AtomicBool::new(false);
    Arc::new(move |_error| {
        if stopping.swap(true, Ordering::SeqCst) { return; }
        let message = format!("Session worker lost its session lock ({}); stopping so a second owner is not overwritten", FileLockLostError::CODE);
        let report = stop.report.clone(); let exit = stop.exit.clone(); let timeout = stop.report_timeout;
        tokio::spawn(async move {
            bounded_report(report(message), timeout).await;
            exit(1);
        });
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::{mpsc, oneshot};
    fn lost() -> FileLockLostError { FileLockLostError { path: "session.jsonl".into(), cause: "ECOMPROMISED".into() } }

    #[tokio::test]
    async fn reports_once_before_exit_and_never_closes_a_harness() {
        let (reports, mut received) = mpsc::unbounded_channel();
        let (exit, exited) = oneshot::channel();
        let exit = std::sync::Mutex::new(Some(exit));
        let stop = stop_worker_on_lost_ownership(OwnershipLossStop::new(move |message| {
            let reports = reports.clone(); Box::pin(async move { reports.send(message).map_err(|error| error.to_string()) })
        }, move |code| { exit.lock().expect("exit sender").take().expect("one exit").send(code).expect("exit listener"); }));
        stop(lost()); stop(lost());
        let message = tokio::time::timeout(Duration::from_secs(2), received.recv()).await.expect("report deadline").expect("report");
        assert!(message.contains(FileLockLostError::CODE));
        assert_eq!(tokio::time::timeout(Duration::from_secs(2), exited).await.expect("exit deadline").expect("exit"), 1);
        assert!(received.try_recv().is_err());
    }

    #[tokio::test]
    async fn report_failure_still_exits() {
        let (exit, exited) = oneshot::channel(); let exit = std::sync::Mutex::new(Some(exit));
        let stop = stop_worker_on_lost_ownership(OwnershipLossStop::new(|_| Box::pin(async { Err("disconnected".into()) }), move |code| {
            exit.lock().expect("exit sender").take().expect("one exit").send(code).expect("exit listener");
        }));
        stop(lost());
        assert_eq!(tokio::time::timeout(Duration::from_secs(2), exited).await.expect("exit deadline").expect("exit"), 1);
    }

    #[tokio::test]
    async fn blocked_report_cannot_hold_the_worker_open() {
        let (exit, exited) = oneshot::channel(); let exit = std::sync::Mutex::new(Some(exit));
        let mut options = OwnershipLossStop::new(|_| Box::pin(std::future::pending()), move |code| {
            exit.lock().expect("exit sender").take().expect("one exit").send(code).expect("exit listener");
        });
        // Time is the behavior under test: the report never completes, so only its deadline exits.
        options.report_timeout = Duration::from_millis(1);
        let stop = stop_worker_on_lost_ownership(options); stop(lost());
        assert_eq!(tokio::time::timeout(Duration::from_secs(2), exited).await.expect("exit deadline").expect("exit"), 1);
    }
}
