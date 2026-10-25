use maho_ext_api::AbortSignal;
use notify::Watcher;
use std::{path::Path, time::Duration};

#[derive(Debug, PartialEq, Eq)]
pub enum SentinelWaitResult {
    Present,
    Timeout,
}

/// Waits for a run's state to become ready, re-arming the deadline whenever the state changes.
///
/// `deadline_at` is read again on every event and every recheck, so a queued run (no deadline) waits
/// without one and an admitted run arms the deadline its supervisor published. A changed deadline
/// replaces the armed timer instead of adding a second one.
async fn wait_for_run_state(
    directory: &Path,
    ready: impl Fn() -> bool,
    mut deadline_at: impl FnMut() -> Option<f64>,
    now: impl Fn() -> f64,
    signal: Option<&AbortSignal>,
) -> SentinelWaitResult {
    if signal.is_some_and(AbortSignal::is_aborted) {
        return SentinelWaitResult::Timeout;
    }
    if ready() {
        return SentinelWaitResult::Present;
    }
    let abort = async {
        match signal {
            Some(signal) => signal.cancelled().await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(abort);
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let _ = events.send(event);
    })
    .ok();
    if watcher.as_mut().is_some_and(|watcher| {
        watcher
            .watch(directory, notify::RecursiveMode::NonRecursive)
            .is_err()
    }) {
        watcher = None;
    }
    // With a live watcher the re-check is only a safety net; without one it is the sole observer.
    let recheck_every = |watched: bool| Duration::from_millis(if watched { 1000 } else { 25 });
    let mut recheck = tokio::time::interval_at(
        tokio::time::Instant::now() + recheck_every(watcher.is_some()),
        recheck_every(watcher.is_some()),
    );
    let mut armed: Option<Option<f64>> = None;
    let mut deadline: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;
    loop {
        if ready() {
            return SentinelWaitResult::Present;
        }
        let current = deadline_at();
        if current.is_some_and(|at| at <= now()) {
            return SentinelWaitResult::Timeout;
        }
        if armed != Some(current) {
            armed = Some(current);
            deadline = current.map(|at| {
                Box::pin(tokio::time::sleep(Duration::from_secs_f64(
                    ((at - now()).max(0.0)) / 1000.0,
                )))
            });
        }
        let armed_sleep = async {
            match deadline.as_mut() {
                Some(sleep) => sleep.await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(armed_sleep);
        tokio::select! {
            _ = &mut abort => return SentinelWaitResult::Timeout,
            _ = &mut armed_sleep => return SentinelWaitResult::Timeout,
            event = received.recv(), if watcher.is_some() => match event {
                Some(Ok(_)) => {},
                _ => {
                    watcher = None;
                    recheck = tokio::time::interval_at(tokio::time::Instant::now() + recheck_every(false), recheck_every(false));
                }
            },
            _ = recheck.tick() => {},
        }
    }
}

pub async fn wait_for_run_sentinel(
    path: &Path,
    deadline_at: f64,
    now: impl Fn() -> f64,
    signal: Option<&AbortSignal>,
) -> SentinelWaitResult {
    wait_for_run_state(
        path.parent().unwrap_or(Path::new(".")),
        || path.exists(),
        || Some(deadline_at),
        now,
        signal,
    )
    .await
}

pub async fn wait_for_run_completion(
    outcome: &Path,
    launch: &Path,
    matching: impl Fn() -> bool,
    deadline: impl FnMut() -> Option<f64>,
    now: impl Fn() -> f64,
    signal: Option<&AbortSignal>,
) -> SentinelWaitResult {
    wait_for_run_state(
        outcome.parent().unwrap_or(Path::new(".")),
        || outcome.exists() && !launch.exists() && matching(),
        deadline,
        now,
        signal,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    #[tokio::test(start_paused = true)]
    async fn missing_directory_reaches_deadline() {
        assert_eq!(
            wait_for_run_sentinel(
                Path::new("/missing-memory-sentinel/outcome.json"),
                0.0,
                || 0.0,
                None
            )
            .await,
            SentinelWaitResult::Timeout
        );
    }
    #[tokio::test(start_paused = true)]
    async fn stale_attempt_does_not_complete_before_current_publication() {
        let root = tempfile::tempdir().unwrap();
        let outcome = root.path().join("outcome.json");
        let launch = root.path().join("launch.json");
        super::super::run_artifacts::write_run_json_atomic(
            &outcome,
            &serde_json::json!({"attempt":1}),
            0o600,
        )
        .unwrap();
        let wait = wait_for_run_completion(
            &outcome,
            &launch,
            || {
                super::super::run_artifacts::read_run_json::<serde_json::Value>(&outcome).unwrap()["attempt"]
                    == 2
            },
            || Some(5000.0),
            || 0.0,
            None,
        );
        tokio::pin!(wait);
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(wait.as_mut().poll(cx).is_pending()))
                .await
        );
        super::super::run_artifacts::write_run_json_atomic(
            &outcome,
            &serde_json::json!({"attempt":2}),
            0o600,
        )
        .unwrap();
        assert_eq!(wait.await, SentinelWaitResult::Present);
    }
    #[tokio::test]
    async fn abort_resolves_pending_wait_without_deadline_delay() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("outcome.json");
        let signal = AbortSignal::default();
        let wait = wait_for_run_sentinel(&path, 5000.0, || 0.0, Some(&signal));
        tokio::pin!(wait);
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(wait.as_mut().poll(cx).is_pending()))
                .await
        );
        signal.abort();
        assert_eq!(wait.await, SentinelWaitResult::Timeout);
    }
    #[tokio::test]
    async fn directory_event_observes_atomic_publication() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("outcome.json");
        let wait = wait_for_run_sentinel(&path, 5000.0, || 0.0, None);
        tokio::pin!(wait);
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(wait.as_mut().poll(cx).is_pending()))
                .await
        );
        super::super::run_artifacts::write_run_json_atomic(
            &path,
            &serde_json::json!({"attempt":1}),
            0o600,
        )
        .unwrap();
        assert_eq!(wait.await, SentinelWaitResult::Present);
    }
    #[tokio::test]
    async fn a_queued_run_waits_past_its_original_deadline() {
        let root = tempfile::tempdir().unwrap();
        let outcome = root.path().join("outcome.json");
        let launch = root.path().join("launch.json");
        let armed = std::cell::Cell::new(Some(1000.0));
        let wait =
            wait_for_run_completion(&outcome, &launch, || true, || armed.get(), || 0.0, None);
        tokio::pin!(wait);
        // The queued run publishes no deadline at all: the wait stays pending instead of expiring.
        armed.set(None);
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(wait.as_mut().poll(cx).is_pending()))
                .await
        );
        armed.set(Some(50_000.0));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(wait.as_mut().poll(cx).is_pending()))
                .await
        );
        super::super::run_artifacts::write_run_json_atomic(
            &outcome,
            &serde_json::json!({"attempt":1}),
            0o600,
        )
        .unwrap();
        assert_eq!(wait.await, SentinelWaitResult::Present);
    }
    #[tokio::test(start_paused = true)]
    async fn an_already_past_dynamic_deadline_times_out() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("outcome.json");
        assert_eq!(
            wait_for_run_completion(
                &path,
                &root.path().join("launch.json"),
                || true,
                || Some(0.0),
                || 1000.0,
                None
            )
            .await,
            SentinelWaitResult::Timeout
        );
    }
}
