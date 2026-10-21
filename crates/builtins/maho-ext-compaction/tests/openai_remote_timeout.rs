use std::{sync::atomic::{AtomicBool, Ordering}, time::Duration};
use maho_ai::utils::abort::AbortController;
use maho_ext_compaction::openai_remote_timeout::run_with_remote_timeout;

#[tokio::test(start_paused = true)]
async fn timeout_aborts_operation_and_notifies_once() {
    let source = AbortController::new();
    let signal = source.signal();
    let captured = std::sync::Mutex::new(None);
    let notified = AtomicBool::new(false);
    let acknowledged = AtomicBool::new(false);
    let acknowledgement = &acknowledged;
    let (cleanup_tx, cleanup_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let operation = run_with_remote_timeout(&signal, Duration::from_secs(1), |signal| {
        *captured.lock().unwrap() = Some(signal.clone());
        async move {
            signal.cancelled().await;
            cleanup_tx.send(()).expect("cleanup subscriber");
            release_rx.await.expect("cleanup release");
            acknowledgement.store(true, Ordering::SeqCst);
            Ok::<(), &'static str>(())
        }
    }, || {
        assert!(acknowledged.load(Ordering::SeqCst));
        assert!(!notified.swap(true, Ordering::SeqCst));
    }, || "aborted");
    tokio::pin!(operation);
    tokio::select! {
        result = &mut operation => panic!("timeout returned before cleanup release: {result:?}"),
        cleanup = tokio::time::timeout(Duration::from_secs(5), cleanup_rx) => {
            cleanup.expect("cleanup deadline").expect("cleanup started");
        }
    }
    assert!(!acknowledged.load(Ordering::SeqCst));
    assert!(!notified.load(Ordering::SeqCst));
    release_tx.send(()).expect("cleanup release subscriber");
    let result = tokio::time::timeout(Duration::from_secs(5), operation).await.expect("timeout settlement deadline");
    assert_eq!(result, Ok(None));
    assert!(acknowledged.load(Ordering::SeqCst));
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
