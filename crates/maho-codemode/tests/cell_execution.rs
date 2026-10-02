use std::sync::Arc;
use maho_ai::utils::abort::{AbortController,AbortReason};
use maho_codemode::tool::cell_execution::CellExecution;

#[tokio::test]
async fn caller_cancellation_rejects_wait_and_fences_late_results() {
    let caller=AbortController::new();
    let (sender,mut aborts)=tokio::sync::mpsc::unbounded_channel();
    let execution=CellExecution::new(caller.signal(),"cell".into(),None,Arc::new(move |reason|{sender.send(reason).expect("abort receiver");}));
    caller.abort(Some(AbortReason::new("AbortError","cancelled")));
    assert_eq!(aborts.recv().await.expect("abort event").message,"cancelled");
    assert_eq!(execution.wait(async {Ok(42)}).await,Err("cancelled".into()));
}

#[tokio::test]
async fn detach_resolves_without_aborting_and_finish_removes_caller_listener() {
    let caller=AbortController::new();
    let (sender,mut aborts)=tokio::sync::mpsc::unbounded_channel();
    let execution=CellExecution::new(caller.signal(),"cell".into(),None,Arc::new(move |reason|{sender.send(reason).expect("abort receiver");}));
    let mut detached=execution.detached();
    execution.detach();
    detached.changed().await.expect("detach event");
    assert!(*detached.borrow());
    assert_eq!(execution.wait(async {Ok(42)}).await,Ok(42));
    execution.finish();
    caller.abort(None);
    assert!(matches!(aborts.try_recv(),Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
}

#[tokio::test(start_paused = true)]
async fn bridge_pause_cannot_extend_the_foreground_watchdog_deadline() {
    use maho_codemode::tool::cell_execution::CellIdleWatchdogOptions;
    let caller=AbortController::new();
    let (sender,mut expiries)=tokio::sync::mpsc::unbounded_channel();
    let execution=CellExecution::new(caller.signal(),"cell".into(),Some(CellIdleWatchdogOptions {timeout_ms:20,max_pause_grace_ms:60,on_timeout:Arc::new(move |error|{sender.send(error).expect("expiry receiver");})}),Arc::new(|_|{}));
    execution.pause();
    tokio::time::advance(std::time::Duration::from_millis(60)).await;
    assert!(expiries.recv().await.expect("deadline expiry").contains("waiting on a host tool call"));
    execution.finish();
}
