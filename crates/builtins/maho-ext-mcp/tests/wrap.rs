use std::sync::{Arc,Mutex};
use maho_ext_mcp::{wrap::*,errors::{McpError,McpErrorKind},log::McpLogger};
fn error()->McpError {McpError::new(McpErrorKind::Protocol,"wrapped boom")}
#[tokio::test]
async fn callback_error_reaches_logger_and_notify_without_rejection() {
    let entries=Arc::new(Mutex::new(Vec::new()));let records=entries.clone();let messages=Arc::new(Mutex::new(Vec::new()));let notifications=messages.clone();
    let sink=McpAsyncErrorSink {logger:Arc::new(move|scope,data|{records.lock().unwrap().push((scope.to_owned(),data["message"].as_str().unwrap().to_owned()));Ok(())}),notify:Some(Arc::new(move|message,_|{notifications.lock().unwrap().push(message);Box::pin(async{Ok(())})}))};
    wrap_async("unit.wrap",async{Err(error())},&sink).await;
    assert_eq!(*entries.lock().unwrap(),vec![("unit.wrap".into(),"wrapped boom".into())]);assert_eq!(*messages.lock().unwrap(),vec!["MCP unit.wrap failed: wrapped boom"]);
}
#[tokio::test]
async fn production_logger_records_wrapped_error() {
    let root=tempfile::tempdir().unwrap();let logger=Arc::new(Mutex::new(McpLogger::new("prod",root.path(),None).unwrap()));
    wrap_async("prod.scope",async{Err(error())},&McpAsyncErrorSink::from_logger(logger.clone())).await;
    let logger=logger.lock().unwrap();assert!(logger.get_ring_buffer()[0].contains("wrapped boom"));assert!(std::fs::read_to_string(&logger.file_path).unwrap().contains("prod.scope"));
}
#[tokio::test]
async fn notification_failure_is_logged_under_notify_scope() {
    let entries=Arc::new(Mutex::new(Vec::new()));let records=entries.clone();
    let sink=McpAsyncErrorSink {logger:Arc::new(move|scope,_|{records.lock().unwrap().push(scope.to_owned());Ok(())}),notify:Some(Arc::new(|_,_|Box::pin(async{Err(error())})))};
    wrap_async("unit",async{Err(error())},&sink).await;assert_eq!(*entries.lock().unwrap(),vec!["unit","unit.notify"]);
}
#[tokio::test(start_paused=true)]
async fn timer_errors_are_guarded_and_observable() {
    let (sender,receiver)=tokio::sync::oneshot::channel();let sender=Mutex::new(Some(sender));
    let sink=McpAsyncErrorSink {logger:Arc::new(move|_,_|{sender.lock().unwrap().take().unwrap().send(()).unwrap();Ok(())}),notify:None};
    let timer=safe_timer("timer".into(),std::time::Duration::from_millis(10),||async{Err(error())},sink);receiver.await.unwrap();timer.await.unwrap();
}
#[tokio::test(start_paused=true)]
async fn interval_waits_for_first_tick_and_continues_after_error() {
    let (sender,mut receiver)=tokio::sync::mpsc::unbounded_channel();let started=tokio::time::Instant::now();
    let sink=McpAsyncErrorSink {logger:Arc::new(move|_,_|{sender.send(tokio::time::Instant::now()).unwrap();Ok(())}),notify:None};
    let task=safe_interval("interval".into(),std::time::Duration::from_millis(10),||async{Err(error())},sink);
    assert_eq!(receiver.recv().await.unwrap()-started,std::time::Duration::from_millis(10));assert_eq!(receiver.recv().await.unwrap()-started,std::time::Duration::from_millis(20));task.abort();let _=task.await;
}
#[tokio::test]
async fn panicking_callback_is_reported_without_rejecting_the_task() {
    let entries=Arc::new(Mutex::new(Vec::new()));let records=entries.clone();
    let sink=McpAsyncErrorSink {logger:Arc::new(move|scope,data|{records.lock().unwrap().push((scope.to_owned(),data["message"].as_str().unwrap().to_owned()));Ok(())}),notify:None};
    wrap_async("panic",async {panic!("callback failure");},&sink).await;
    assert_eq!(*entries.lock().unwrap(),vec![("panic".into(),"callback failure".into())]);
}
#[tokio::test]
async fn panicking_notification_is_reported_under_notify_scope() {
    let entries=Arc::new(Mutex::new(Vec::new()));let records=entries.clone();
    let sink=McpAsyncErrorSink {logger:Arc::new(move|scope,_|{records.lock().unwrap().push(scope.to_owned());Ok(())}),notify:Some(Arc::new(|_,_|panic!("notification failure")))};
    wrap_async("unit",async {Err(error())},&sink).await;
    assert_eq!(*entries.lock().unwrap(),vec!["unit","unit.notify"]);
}
#[tokio::test]
async fn panicking_logger_does_not_prevent_error_notification() {
    let (sender,receiver)=tokio::sync::oneshot::channel();let sender=Mutex::new(Some(sender));
    let sink=McpAsyncErrorSink {logger:Arc::new(|_,_|panic!("logger failure")),notify:Some(Arc::new(move|_,_|{sender.lock().unwrap().take().unwrap().send(()).unwrap();Box::pin(async {Ok(())})}))};
    wrap_async("unit",async {Err(error())},&sink).await;receiver.await.unwrap();
}
