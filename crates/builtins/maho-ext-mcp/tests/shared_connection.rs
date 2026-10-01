use std::{sync::{Arc,Mutex},time::Duration};
use maho_ext_mcp::{config_schema::{McpServerConfig,Transport},connection::ServerConnection,log::McpLogger,shared_connection::SharedMcpConnection};
#[tokio::test(start_paused=true)]
async fn last_owner_idle_disposal_waits_for_in_flight_request_cancellation() {
    let root=tempfile::tempdir().unwrap();let physical=ServerConnection::new("shared",McpServerConfig {enabled:Some(true),..Default::default()},None,Arc::new(Mutex::new(McpLogger::new("shared",root.path(),None).unwrap())));
    let shared=SharedMcpConnection::with_idle_timeout(physical.clone(),root.path().into(),"hash".into(),Duration::from_secs(5),Duration::from_secs(60));
    let registry=maho_ext_mcp::host_registry::HostMcpRegistry::default();let lease=registry.attach_shared("key",1,||shared.clone()).unwrap();let (started,ready)=tokio::sync::oneshot::channel();let active=shared.clone();
    let task=tokio::spawn(async move {active.run_request(async {started.send(()).unwrap();std::future::pending::<()>().await}).await;});
    ready.await.unwrap();lease.dispose();assert_eq!(shared.lease_count(),0);assert_eq!(physical.state(),maho_ext_mcp::connection::ServerConnectionState::Idle);
    let mut changes=physical.on_state_change();task.abort();assert!(task.await.unwrap_err().is_cancelled());
    let event=tokio::time::timeout(Duration::from_secs(61),changes.recv()).await.unwrap().unwrap();assert_eq!(event.state,maho_ext_mcp::connection::ServerConnectionState::Disabled);
    let replacement=ServerConnection::new("replacement",McpServerConfig {enabled:Some(true),..Default::default()},None,Arc::new(Mutex::new(McpLogger::new("replacement",root.path(),None).unwrap())));
    let replacement_shared=SharedMcpConnection::new(replacement.clone(),root.path().into(),"hash".into(),Duration::from_secs(5));let replacement_lease=registry.attach_shared("key",2,||replacement_shared.clone()).unwrap();assert!(!Arc::ptr_eq(&replacement_lease.shared,&shared));
    registry.dispose().await.unwrap();assert_eq!(replacement_lease.state(),maho_ext_mcp::connection::ServerConnectionState::Disabled);
    shared.dispose().await.unwrap();
}
#[tokio::test]
async fn renewing_one_lease_never_replaces_the_other_owners_process() {
    let root=tempfile::tempdir().unwrap();let config=McpServerConfig {enabled:Some(true),transport:Some(Transport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into()]),..Default::default()};
    let physical=ServerConnection::new("shared",config,None,Arc::new(Mutex::new(McpLogger::new("shared",root.path(),None).unwrap())));
    let shared=SharedMcpConnection::new(physical.clone(),root.path().into(),"hash".into(),Duration::from_secs(5));let first=shared.attach(1,"key".into()).unwrap();let second=shared.attach(2,"key".into()).unwrap();
    first.connect().await.unwrap();let pid=physical.get_root_pid();let generation=second.generation();first.renew().await.unwrap();assert_eq!(second.generation(),generation);assert_eq!(physical.get_root_pid(),pid);
    assert!(!first.catalog().await.unwrap().tools.is_empty());first.dispose();assert!(first.request("ping",serde_json::json!({}),Duration::from_secs(5)).await.is_err());
    second.request("ping",serde_json::json!({}),Duration::from_secs(5)).await.unwrap();assert_eq!(shared.lease_count(),1);second.dispose();assert_eq!(shared.lease_count(),0);shared.dispose().await.unwrap();
}
