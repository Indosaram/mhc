use std::{sync::Arc,collections::BTreeMap};
use maho_ext_mcp::{service::McpService,host_registry::HostMcpRegistry,service_types::{McpLifecycleState,McpUnspawnedState}};
#[tokio::test]
async fn repeated_attach_retains_connections_and_disabled_config_detaches_them() {
    let root=tempfile::tempdir().unwrap();let cwd=tempfile::tempdir().unwrap();let registry=Arc::new(HostMcpRegistry::default());let mut service=McpService::new(registry.clone(),1);
    service.attach_session(cwd.path(),root.path(),&BTreeMap::new(),true,&[]).await.unwrap();assert!(service.server_snapshots().await.is_empty());
    let declaration=maho_ext_api::RegisteredMcpServerDeclaration {name:"fixture".into(),config:maho_ext_api::McpServerDeclaration {transport:Some(maho_ext_api::McpTransport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into()]),..Default::default()},extension_path:"fixture-extension".into(),registration_cwd:cwd.path().into()};
    service.attach_session(cwd.path(),root.path(),&BTreeMap::new(),true,std::slice::from_ref(&declaration)).await.unwrap();assert_eq!(registry.size(),1);
    let physical=service.connections["fixture"].entry.lock().await.connection.clone();service.connect_server("fixture").await.unwrap();assert!(!matches!(service.server_snapshots().await[0].lifecycle_state,McpLifecycleState::Other(McpUnspawnedState::NotSpawned)));
    service.attach_session(cwd.path(),root.path(),&BTreeMap::new(),true,std::slice::from_ref(&declaration)).await.unwrap();assert!(Arc::ptr_eq(&physical,&service.connections["fixture"].entry.lock().await.connection));
    let mut disabled=declaration;disabled.config.enabled=Some(false);service.attach_session(cwd.path(),root.path(),&BTreeMap::new(),true,&[disabled]).await.unwrap();assert_eq!(registry.size(),0);assert!(service.connections.is_empty());
    service.dispose().await.unwrap();
    let eager=maho_ext_api::RegisteredMcpServerDeclaration {name:"eager".into(),config:maho_ext_api::McpServerDeclaration {transport:Some(maho_ext_api::McpTransport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into()]),lifecycle:Some(maho_ext_api::McpLifecycle::Eager),startup_timeout_ms:Some(0.0),..Default::default()},extension_path:"fixture-extension".into(),registration_cwd:cwd.path().into()};
    service.attach_session(cwd.path(),root.path(),&BTreeMap::new(),true,&[eager]).await.unwrap();
    assert_eq!(service.wait_for_deferred_attach(std::time::Duration::from_secs(15)).await,maho_ext_mcp::startup_race::McpStartupRaceResult::Settled);assert!(service.connections["eager"].entry.lock().await.cached_catalog.is_some());service.dispose().await.unwrap();
}
