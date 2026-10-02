use std::{sync::{Arc,Mutex},time::Duration};
use maho_ext_mcp::{connection::*,config_schema::*,log::McpLogger};
fn config()->McpServerConfig {McpServerConfig {enabled:Some(true),transport:Some(Transport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into(),"--tools".into(),"2".into()]),connect_timeout_ms:Some(5000.0),..Default::default()}}
#[tokio::test]
async fn concurrent_connects_share_one_client_and_transitions() {
    let root=tempfile::tempdir().unwrap();let connection=ServerConnection::new("fixture",config(),None,Arc::new(Mutex::new(McpLogger::new("fixture",root.path(),None).unwrap())));let mut states=connection.on_state_change();let mut tools=connection.on_tools_changed();
    let (first,second)=tokio::join!(connection.connect(),connection.connect());let first=first.unwrap();assert!(Arc::ptr_eq(&first,&second.unwrap()));assert_eq!(connection.state(),ServerConnectionState::Connected);
    assert_eq!(states.recv().await.unwrap().state,ServerConnectionState::Connecting);assert_eq!(states.recv().await.unwrap().state,ServerConnectionState::Connected);assert!(states.try_recv().is_err());assert_eq!(tools.recv().await.unwrap().generation,0);
    assert_eq!(first.request("tools/list",serde_json::json!({}),Duration::from_secs(3)).await.unwrap()["tools"].as_array().unwrap().len(),2);connection.dispose().await.unwrap();
}
#[tokio::test]
async fn failed_connect_retains_degraded_error() {
    let root=tempfile::tempdir().unwrap();let mut config=config();config.command=Some("/missing/fixture-command".into());let connection=ServerConnection::new("failure",config,None,Arc::new(Mutex::new(McpLogger::new("failure",root.path(),None).unwrap())));
    assert!(connection.connect().await.is_err());assert_eq!(connection.state(),ServerConnectionState::Degraded);assert!(connection.last_error().is_some());connection.dispose().await.unwrap();
}
#[tokio::test]
async fn invalid_transport_configuration_releases_single_flight() {
    let root=tempfile::tempdir().unwrap();let mut config=config();config.command=None;
    let connection=ServerConnection::new("failure",config,None,Arc::new(Mutex::new(McpLogger::new("failure",root.path(),None).unwrap())));
    for _ in 0..2 {
        connection.mark_failure(ServerConnectionState::Idle,None);
        let mut states=connection.on_state_change();
        assert!(tokio::time::timeout(Duration::from_secs(3),connection.connect()).await.unwrap().is_err());
        assert_eq!(connection.state(),ServerConnectionState::Degraded);assert!(connection.last_error().is_some());
        assert_eq!(states.try_recv().unwrap().state,ServerConnectionState::Degraded);
    }
    connection.dispose().await.unwrap();
}
#[tokio::test]
async fn disable_clears_error_and_rejects_connect() {
    let root=tempfile::tempdir().unwrap();let connection=ServerConnection::new("fixture",config(),None,Arc::new(Mutex::new(McpLogger::new("fixture",root.path(),None).unwrap())));connection.mark_failure(ServerConnectionState::Suspended,None);connection.disable().await.unwrap();assert!(connection.connect().await.is_err());assert_eq!(connection.generation(),1);assert!(connection.last_error().is_none());
}
#[tokio::test]
async fn list_changed_after_call_reaches_connection() {
    let root=tempfile::tempdir().unwrap();let mut config=config();config.args.as_mut().unwrap().push("--emit-list-changed".into());
    let connection=ServerConnection::new("fixture",config,None,Arc::new(Mutex::new(McpLogger::new("fixture",root.path(),None).unwrap())));
    let client=connection.connect().await.unwrap();let mut changes=connection.on_tools_changed();
    client.request("tools/call",serde_json::json!({"name":"tool_1","arguments":{"value":"go"}}),Duration::from_secs(3)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3),changes.recv()).await.unwrap().unwrap();connection.dispose().await.unwrap();
}
