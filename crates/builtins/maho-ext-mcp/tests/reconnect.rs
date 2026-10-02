use std::sync::{Arc,Mutex,atomic::{AtomicUsize,Ordering}};
use maho_ext_mcp::{connection::*,config_schema::*,log::McpLogger,reconnect::*,errors::{McpError,McpErrorKind}};
#[tokio::test(start_paused=true)]
async fn repeated_failures_open_the_breaker_and_manual_reconnect_resets_it() {
    let root=tempfile::tempdir().unwrap();let connection=ServerConnection::new("retry",McpServerConfig {enabled:Some(true),..Default::default()},None,Arc::new(Mutex::new(McpLogger::new("retry",root.path(),None).unwrap())));
    let attempts=Arc::new(AtomicUsize::new(0));let count=attempts.clone();
    let reconnect=McpReconnect::configure(connection.clone(),Arc::new(move ||{count.fetch_add(1,Ordering::SeqCst);Box::pin(async {Err(McpError::new(McpErrorKind::Connect,"failed"))})}),Arc::new(||true),Arc::new(||1.0));
    let mut changes=connection.on_state_change();connection.mark_failure(ServerConnectionState::Degraded,Some(McpError::new(McpErrorKind::Connect,"initial")));
    tokio::time::timeout(std::time::Duration::from_secs(30),async {loop {if changes.recv().await.unwrap().state==ServerConnectionState::Suspended {break;}}}).await.unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst),5);assert_eq!(reconnect.attempts_in_window(),5);
    assert!(reconnect.reconnect_now().await.is_err());assert_eq!(reconnect.attempts_in_window(),1);
    reconnect.dispose();connection.dispose().await.unwrap();
}
