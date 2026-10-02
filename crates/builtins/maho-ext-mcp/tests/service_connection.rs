use maho_ext_mcp::{config_schema::{McpServerConfig,Transport},host_registry::HostMcpRegistry,service_connection::*,startup_race::connect_and_refresh_mcp_catalog};
#[tokio::test]
async fn session_connection_refreshes_catalog_and_disposes_registry_owner() {
    let root=tempfile::tempdir().unwrap();let registry=HostMcpRegistry::default();
    let config=McpServerConfig {enabled:Some(true),transport:Some(Transport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into()]),..Default::default()};
    let session=create_mcp_session_connection(SessionConnectionOptions {registry:&registry,owner:1,key:"key",name:"fixture",config_hash:"hash",config:config.clone(),agent_dir:root.path(),env:None}).unwrap();
    {let mut entry=session.entry.lock().await;connect_and_refresh_mcp_catalog(&mut entry,&config).await.unwrap();
        assert_eq!(entry.connection.state(),maho_ext_mcp::connection::ServerConnectionState::Connected);assert!(entry.cache_refreshed_after_connect);
        assert!(!entry.cached_catalog.as_ref().unwrap().tools.is_empty());
        let cache=maho_ext_mcp::catalog_cache::read_mcp_catalog_cache(root.path());assert_eq!(cache.servers["fixture"].config_hash,"hash");}
    dispose_entry_connection(&session,&registry,1).await.unwrap();assert_eq!(registry.size(),0);
}
