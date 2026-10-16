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
    let started = Instant::now();
    tokio::select! {
        result = operation => result.map(Some),
        () = tokio::time::sleep_until(deadline) => {
            controller.abort(None);
            on_timeout(started.elapsed());
            Ok(None)
        }
    }
}

pub struct RemoteCompactionDeadline {
    deadline: tokio::time::Instant,
    attempt_budget: Duration,
}

impl Clone for RemoteCompactionDeadline {
    fn clone(&self) -> Self { Self { deadline: self.deadline, attempt_budget: self.attempt_budget } }
}

impl RemoteCompactionDeadline {
    pub fn new(attempt_budget: Duration) -> Self {
        Self { deadline: tokio::time::Instant::now() + attempt_budget, attempt_budget }
    }

    pub fn remaining(&self) -> Duration { self.deadline.saturating_duration_since(tokio::time::Instant::now()) }

    pub fn next_attempt(&self) -> Duration { self.attempt_budget.min(self.remaining()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deadline_budget_is_bounded_by_the_configured_duration() {
        let deadline = RemoteCompactionDeadline::new(Duration::from_millis(20));
        assert!(deadline.next_attempt() <= Duration::from_millis(20));
        tokio::time::timeout(Duration::from_secs(1), tokio::time::sleep(Duration::from_millis(25)))
            .await
            .expect("bounded test timer");
        assert_eq!(deadline.next_attempt(), Duration::ZERO);
    }
}
