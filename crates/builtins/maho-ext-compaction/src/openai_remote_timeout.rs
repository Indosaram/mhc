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
