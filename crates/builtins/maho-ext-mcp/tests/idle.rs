use std::sync::{Arc,Mutex};
use maho_ext_mcp::{connection::ServerConnection,config_schema::McpServerConfig,idle::McpConnectionLifecycle,log::McpLogger};
#[tokio::test]
async fn cancellation_releases_the_in_flight_lifecycle_guard() {
    let root=tempfile::tempdir().unwrap();let config=McpServerConfig {enabled:Some(true),..Default::default()};
    let connection=ServerConnection::new("idle",config.clone(),None,Arc::new(Mutex::new(McpLogger::new("idle",root.path(),None).unwrap())));
    let lifecycle=McpConnectionLifecycle::configure(connection.clone(),config);
    let (started,signal)=tokio::sync::oneshot::channel();let active=lifecycle.clone();
    let call=tokio::spawn(async move {active.run_call(async {started.send(()).unwrap();std::future::pending::<()>().await}).await;});
    signal.await.unwrap();assert_eq!(lifecycle.in_flight(),1);call.abort();assert!(call.await.unwrap_err().is_cancelled());assert_eq!(lifecycle.in_flight(),0);
    lifecycle.dispose();connection.dispose().await.unwrap();
}
