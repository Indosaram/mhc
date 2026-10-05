use std::{path::Path, time::Duration};
use maho_ext_api::AbortSignal;
use notify::Watcher;

#[derive(Debug, PartialEq, Eq)]
pub enum SentinelWaitResult { Present, Timeout }

async fn wait_for_run_state(directory: &Path, ready: impl Fn() -> bool, deadline_at: f64, now: impl Fn() -> f64, signal: Option<&AbortSignal>) -> SentinelWaitResult {
    if signal.is_some_and(AbortSignal::is_aborted) { return SentinelWaitResult::Timeout; }
    if ready() { return SentinelWaitResult::Present; }
    let timeout = tokio::time::sleep(Duration::from_secs_f64((deadline_at - now()).max(0.0) / 1000.0));
    tokio::pin!(timeout);
    let mut recheck = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_millis(25), Duration::from_millis(25));
    let abort = async { match signal { Some(signal) => signal.cancelled().await, None => std::future::pending().await } };
    tokio::pin!(abort);
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| { let _ = events.send(event); }).ok();
    if watcher.as_mut().is_some_and(|watcher| watcher.watch(directory, notify::RecursiveMode::NonRecursive).is_err()) { watcher = None; }
    if ready() { return SentinelWaitResult::Present; }
    loop {
        tokio::select! {
            _ = &mut abort => return SentinelWaitResult::Timeout,
            _ = &mut timeout => return SentinelWaitResult::Timeout,
            event = received.recv(), if watcher.is_some() => match event {
                Some(Ok(_)) => if ready() { return SentinelWaitResult::Present; },
                _ => watcher = None,
            },
            _ = recheck.tick() => if ready() { return SentinelWaitResult::Present; },
        }
    }
}

pub async fn wait_for_run_sentinel(path: &Path, deadline_at: f64, now: impl Fn() -> f64, signal: Option<&AbortSignal>) -> SentinelWaitResult {
    wait_for_run_state(path.parent().unwrap_or(Path::new(".")), || path.exists(), deadline_at, now, signal).await
}

pub async fn wait_for_run_completion(outcome: &Path, launch: &Path, matching: impl Fn() -> bool, deadline_at: f64, now: impl Fn() -> f64, signal: Option<&AbortSignal>) -> SentinelWaitResult {
    wait_for_run_state(outcome.parent().unwrap_or(Path::new(".")), || outcome.exists() && !launch.exists() && matching(), deadline_at, now, signal).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    #[tokio::test(start_paused=true)] async fn missing_directory_reaches_deadline() { assert_eq!(wait_for_run_sentinel(Path::new("/missing-memory-sentinel/outcome.json"),0.0,||0.0,None).await,SentinelWaitResult::Timeout); }
    #[tokio::test(start_paused=true)] async fn stale_attempt_does_not_complete_before_current_publication() {
        let root=tempfile::tempdir().unwrap(); let outcome=root.path().join("outcome.json"); let launch=root.path().join("launch.json");
        super::super::run_artifacts::write_run_json_atomic(&outcome,&serde_json::json!({"attempt":1}),0o600).unwrap();
        let wait=wait_for_run_completion(&outcome,&launch,|| super::super::run_artifacts::read_run_json::<serde_json::Value>(&outcome).unwrap()["attempt"]==2,5000.0,||0.0,None);
        tokio::pin!(wait);
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(wait.as_mut().poll(cx).is_pending())).await);
        super::super::run_artifacts::write_run_json_atomic(&outcome,&serde_json::json!({"attempt":2}),0o600).unwrap();
        assert_eq!(wait.await,SentinelWaitResult::Present);
    }
    #[tokio::test] async fn abort_resolves_pending_wait_without_deadline_delay() {
        let root=tempfile::tempdir().unwrap(); let path=root.path().join("outcome.json"); let signal=AbortSignal::default();
        let wait=wait_for_run_sentinel(&path,5000.0,||0.0,Some(&signal)); tokio::pin!(wait);
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(wait.as_mut().poll(cx).is_pending())).await);
        signal.abort(); assert_eq!(wait.await,SentinelWaitResult::Timeout);
    }
    #[tokio::test] async fn directory_event_observes_atomic_publication() {
        let root=tempfile::tempdir().unwrap(); let path=root.path().join("outcome.json");
        let wait=wait_for_run_sentinel(&path,5000.0,||0.0,None); tokio::pin!(wait);
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(wait.as_mut().poll(cx).is_pending())).await);
        super::super::run_artifacts::write_run_json_atomic(&path,&serde_json::json!({"attempt":1}),0o600).unwrap();
        assert_eq!(wait.await,SentinelWaitResult::Present);
    }
}
