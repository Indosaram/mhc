use std::sync::{Arc,Mutex};
use maho_ext_mcp::{host_registry::*,connection::*,config_schema::*,log::McpLogger};
#[tokio::test]
async fn sharing_preserves_the_connection_until_the_last_owner_detaches() {
    let root=tempfile::tempdir().unwrap();let registry=HostMcpRegistry::default();
    let first=registry.attach("key",1,||ServerConnection::new("server",McpServerConfig {enabled:Some(true),..Default::default()},None,Arc::new(Mutex::new(McpLogger::new("server",root.path(),None).unwrap()))),true);
    let second=registry.attach("key",2,||panic!("shared entry should avoid factory"),true);
    assert!(Arc::ptr_eq(&first,&second));assert_eq!(registry.size(),1);
    registry.detach("key",1).await.unwrap();assert_eq!(first.state(),ServerConnectionState::Idle);
    registry.detach("key",2).await.unwrap();assert_eq!(first.state(),ServerConnectionState::Disabled);assert_eq!(registry.size(),0);
}
#[tokio::test]
async fn duplicate_owner_attach_is_reference_counted() {
    let root=tempfile::tempdir().unwrap();let registry=HostMcpRegistry::default();
    registry.attach("key",1,||ServerConnection::new("server",McpServerConfig::default(),None,Arc::new(Mutex::new(McpLogger::new("server",root.path(),None).unwrap()))),false);
    registry.attach("key",1,||panic!("owned entry should avoid factory"),false);
    registry.detach("key",1).await.unwrap();assert_eq!(registry.size(),1);assert_eq!(registry.for_each_owner("key"),vec![1]);
    registry.detach("key",1).await.unwrap();assert_eq!(registry.size(),0);
    assert!(matches!(registry.detach("key",1).await,Err(RegistryDetachError::Owner(HostMcpRegistryError {code:"unknown_owner",..}))));
}
