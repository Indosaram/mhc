use std::future::Future;
use tokio::time::Instant;
use std::time::Duration;
use maho_ai::utils::abort::{AbortController, AbortSignal, ListenerId};

struct AbortLink<'a> {
    source: &'a AbortSignal,
    listener: ListenerId,
}

impl Drop for AbortLink<'_> {
    fn drop(&mut self) {
        self.source.remove_abort_listener(self.listener);
    }
}

pub async fn run_with_remote_timeout<T, E, F: Future<Output = Result<T, E>>>(
    signal: &AbortSignal,
    timeout: Duration,
    run: impl FnOnce(AbortSignal) -> F,
    on_timeout: impl FnOnce(),
    aborted_error: impl FnOnce() -> E,
) -> Result<Option<T>, E> {
    run_with_remote_deadline(signal, Instant::now() + timeout, run, on_timeout, aborted_error).await
}

pub async fn run_with_remote_deadline<T, E, F: Future<Output = Result<T, E>>>(
    signal: &AbortSignal,
    deadline: Instant,
    run: impl FnOnce(AbortSignal) -> F,
    on_timeout: impl FnOnce(),
    aborted_error: impl FnOnce() -> E,
) -> Result<Option<T>, E> {
    run_with_remote_deadline_report(signal, deadline, run, move |_| on_timeout(), aborted_error).await
}

pub async fn run_with_remote_deadline_report<T, E, F: Future<Output = Result<T, E>>>(
    signal: &AbortSignal,
    deadline: Instant,
    run: impl FnOnce(AbortSignal) -> F,
    on_timeout: impl FnOnce(Duration),
    aborted_error: impl FnOnce() -> E,
) -> Result<Option<T>, E> {
    if signal.aborted() {
        return Err(aborted_error());
    }
    let controller = AbortController::new();
    let linked = controller.clone();
    let listener = signal.add_abort_listener(move |_| linked.abort(None));
    let _link = AbortLink { source: signal, listener };
    let operation = run(controller.signal());
    tokio::pin!(operation);
    let started = Instant::now();
    tokio::select! {
        biased;
        () = signal.cancelled() => {
            controller.abort(None);
            operation.await?;
            Err(aborted_error())
        }
        result = &mut operation => result.map(Some),
        () = tokio::time::sleep_until(deadline) => {
            controller.abort(None);
            operation.await?;
            on_timeout(started.elapsed());
            Ok(None)
        }
    }
}

const TOTAL_BUDGET: Duration = Duration::from_millis(maho_core::compaction::stream_watchdog::SUMMARIZATION_TOTAL_BUDGET_MS as u64);

pub struct RemoteCompactionDeadline {
    deadline: tokio::time::Instant,
    attempt_budget: Duration,
}

impl Clone for RemoteCompactionDeadline {
    fn clone(&self) -> Self { Self { deadline: self.deadline, attempt_budget: self.attempt_budget } }
}

impl RemoteCompactionDeadline {
    /// senpi `createRemoteCompactionDeadline`: every attempt shares one deadline of
    /// `max(SUMMARIZATION_TOTAL_BUDGET_MS, attemptBudget)`.
    pub fn new(attempt_budget: Duration) -> Self {
        Self { deadline: tokio::time::Instant::now() + attempt_budget.max(TOTAL_BUDGET), attempt_budget }
    }

    /// Same shared deadline, with a different per-attempt budget.
    pub fn with_attempt_budget(&self, attempt_budget: Duration) -> Self {
        Self { deadline: self.deadline, attempt_budget }
    }

    pub fn remaining(&self) -> Duration { self.deadline.saturating_duration_since(tokio::time::Instant::now()) }

    pub fn next_attempt(&self) -> Duration { self.attempt_budget.min(self.remaining()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn deadline_is_shared_across_attempts_and_floored_at_the_total_budget() {
        // Given a small per-attempt budget.
        let deadline = RemoteCompactionDeadline::new(Duration::from_millis(20));
        assert_eq!(deadline.next_attempt(), Duration::from_millis(20));
        // When the attempt budget has elapsed, the shared 900s deadline still has room.
        tokio::time::advance(Duration::from_millis(25)).await;
        assert_eq!(deadline.next_attempt(), Duration::from_millis(20));
        // A larger attempt budget stretches the deadline past the total budget.
        assert_eq!(RemoteCompactionDeadline::new(Duration::from_secs(1000)).remaining(), Duration::from_secs(1000));
        // Then once the total budget is spent no attempt can run.
        tokio::time::advance(Duration::from_secs(900)).await;
        assert_eq!(deadline.next_attempt(), Duration::ZERO);
    }

    #[tokio::test]
    async fn source_cancellation_waits_for_cleanup_and_preserves_operation_errors() {
        for operation_error in [false, true] {
            // Given a running operation with explicitly gated cancellation cleanup.
            let source = AbortController::new();
            let signal = source.signal();
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (cleanup_tx, cleanup_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                run_with_remote_timeout(&signal, Duration::from_secs(60), |signal| async move {
                    started_tx.send(()).expect("start subscriber");
                    signal.cancelled().await;
                    cleanup_tx.send(()).expect("cleanup subscriber");
                    release_rx.await.expect("release cleanup");
                    if operation_error {Err("operation failed")} else {Ok(7)}
                }, || panic!("source cancellation is not a timeout"), || "aborted").await
            });
            let bound = Duration::from_secs(5);
            tokio::time::timeout(bound, started_rx).await.expect("start deadline").expect("started");
            // When the caller cancels while cleanup is held open.
            source.abort(None);
            tokio::time::timeout(bound, cleanup_rx).await.expect("cleanup deadline").expect("cleanup started");
            // Then cancellation cannot settle until cleanup, retaining operation failures.
            assert!(!task.is_finished());
            release_tx.send(()).expect("release subscriber");
            let outcome = tokio::time::timeout(bound, task).await.expect("settlement deadline").expect("task");
            assert_eq!(outcome, Err(if operation_error {"operation failed"} else {"aborted"}));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_waits_for_cleanup_before_reporting_and_fallback() {
        // Given an expired deadline and cleanup held open by its owner.
        let source = AbortController::new();
        let signal = source.signal();
        let reported = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let notification = std::sync::Arc::clone(&reported);
        let (cleanup_tx, cleanup_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        // When the deadline aborts the operation.
        let task = tokio::spawn(async move {
            run_with_remote_timeout(&signal, Duration::ZERO, |signal| async move {
                signal.cancelled().await;
                cleanup_tx.send(()).expect("cleanup subscriber");
                release_rx.await.expect("release cleanup");
                Ok::<_, &str>(7)
            }, || {notification.store(true, std::sync::atomic::Ordering::SeqCst);}, || "aborted").await
        });
        let bound = Duration::from_secs(5);
        tokio::time::timeout(bound, cleanup_rx).await.expect("cleanup deadline").expect("cleanup started");
        // Then neither timeout notification nor fallback happens before cleanup.
        assert!(!task.is_finished());
        assert!(!reported.load(std::sync::atomic::Ordering::SeqCst));
        release_tx.send(()).expect("release subscriber");
        assert_eq!(tokio::time::timeout(bound, task).await.expect("settlement deadline").expect("task"), Ok(None));
        assert!(reported.load(std::sync::atomic::Ordering::SeqCst));
    }
}
