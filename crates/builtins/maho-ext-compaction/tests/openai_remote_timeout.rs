use std::{future::pending, sync::atomic::{AtomicBool, Ordering}, time::Duration};
use maho_ai::utils::abort::AbortController;
use maho_ext_compaction::openai_remote_timeout::run_with_remote_timeout;

#[tokio::test(start_paused = true)]
async fn timeout_aborts_operation_and_notifies_once() {
    let source = AbortController::new();
    let captured = std::sync::Mutex::new(None);
    let notified = AtomicBool::new(false);
    let result = run_with_remote_timeout(&source.signal(), Duration::from_secs(1), |signal| {
        *captured.lock().unwrap() = Some(signal);
        pending::<Result<(), &'static str>>()
    }, || assert!(!notified.swap(true, Ordering::SeqCst)), || "aborted").await;
    assert_eq!(result, Ok(None));
    assert!(notified.load(Ordering::SeqCst));
    assert!(captured.lock().unwrap().as_ref().unwrap().aborted());
}

#[tokio::test]
async fn already_aborted_source_never_starts_operation() {
    let source = AbortController::new();
    source.abort(None);
    let result = run_with_remote_timeout(&source.signal(), Duration::from_secs(1), |_| async {
        panic!("operation started");
        #[expect(unreachable_code, reason = "the callback must never run")]
        Ok::<(), &str>(())
    }, || panic!("timeout fired"), || "aborted").await;
    assert_eq!(result, Err("aborted"));
}

#[tokio::test]
async fn successful_operation_removes_abort_link() {
    let source = AbortController::new();
    let captured = std::sync::Mutex::new(None);
    let result = run_with_remote_timeout(&source.signal(), Duration::from_secs(1), |signal| {
        *captured.lock().unwrap() = Some(signal);
        async { Ok::<_, &str>(7) }
    }, || panic!("timeout fired"), || "aborted").await;
    source.abort(None);
    assert_eq!(result, Ok(Some(7)));
    assert!(!captured.lock().unwrap().as_ref().unwrap().aborted());
}

#[tokio::test]
async fn source_abort_is_forwarded_and_operation_error_is_preserved() {
    let source = AbortController::new();
    let result = run_with_remote_timeout(&source.signal(), Duration::from_secs(1), |signal| async move {
        source.abort(None);
        signal.cancelled().await;
        Err::<(), _>("operation aborted")
    }, || panic!("timeout fired"), || "initial abort").await;
    assert_eq!(result, Err("operation aborted"));
}
