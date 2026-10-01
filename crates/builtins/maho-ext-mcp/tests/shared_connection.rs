use std::{sync::{Arc,Mutex},time::Duration};
use maho_ext_mcp::{config_schema::{McpServerConfig,Transport},connection::ServerConnection,log::McpLogger,shared_connection::SharedMcpConnection};
#[tokio::test]
async fn renewing_one_lease_never_replaces_the_other_owners_process() {
    let root=tempfile::tempdir().unwrap();let config=McpServerConfig {enabled:Some(true),transport:Some(Transport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into()]),..Default::default()};
    let physical=ServerConnection::new("shared",config,None,Arc::new(Mutex::new(McpLogger::new("shared",root.path(),None).unwrap())));
    let shared=SharedMcpConnection::new(physical.clone(),root.path().into(),"hash".into(),Duration::from_secs(5));let first=shared.attach(1,"key".into()).unwrap();let second=shared.attach(2,"key".into()).unwrap();
    first.connect().await.unwrap();let pid=physical.get_root_pid();let generation=second.generation();first.renew().await.unwrap();assert_eq!(second.generation(),generation);assert_eq!(physical.get_root_pid(),pid);
    assert!(!first.catalog().await.unwrap().tools.is_empty());first.dispose();assert!(first.request("ping",serde_json::json!({}),Duration::from_secs(5)).await.is_err());
    second.request("ping",serde_json::json!({}),Duration::from_secs(5)).await.unwrap();assert_eq!(shared.lease_count(),1);second.dispose();assert_eq!(shared.lease_count(),0);shared.dispose().await.unwrap();
}
