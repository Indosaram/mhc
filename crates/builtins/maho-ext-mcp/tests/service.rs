use std::{sync::Arc,collections::BTreeMap};
use maho_ext_mcp::{service::McpService,host_registry::HostMcpRegistry,service_types::{McpLifecycleState,McpUnspawnedState}};
#[tokio::test]
async fn skill_attachment_retains_matching_connections_and_system_precedence() {
    use maho_ext_mcp::{skills::*,config_schema::*};
    let root=tempfile::tempdir().unwrap();let cwd=tempfile::tempdir().unwrap();let registry=Arc::new(HostMcpRegistry::default());let mut service=McpService::new(registry.clone(),1);
    let mut declared=SkillMcpDeclarations::default();
    declared.servers.insert("skill".into(),SkillServerDecl {raw:serde_json::json!({"command":"/missing/lazy"}),source_path:cwd.path().join("mcp.json"),include_tools_by_skill:Default::default()});
    assert!(service.attach_skill_mcp_servers(&declared).await.unwrap().is_empty());assert!(service.config.is_none());
    let system=maho_ext_api::RegisteredMcpServerDeclaration {name:"system".into(),config:maho_ext_api::McpServerDeclaration {command:Some("/missing/system".into()),..Default::default()},extension_path:"extension".into(),registration_cwd:cwd.path().into()};
    service.attach_session(cwd.path(),root.path(),&BTreeMap::from([("FIXTURE_ENV".into(),"retained".into())]),true,&[system]).await.unwrap();
    service.attach_skill_mcp_servers(&declared).await.unwrap();
    let physical=service.connections["skill"].entry.lock().await.connection.clone();
    let config=service.config.as_ref().unwrap().servers["skill"].config.as_ref().unwrap();assert_eq!(config.exposure,Some(Exposure::Search));assert_eq!(config.direct_tools,Some(DirectTools::Patterns(vec![])));assert!(physical.get_root_pid().is_none());
    service.attach_skill_mcp_servers(&declared).await.unwrap();assert!(Arc::ptr_eq(&physical,&service.connections["skill"].entry.lock().await.connection));
    declared.servers.get_mut("skill").unwrap().raw["command"]=serde_json::json!("/missing/changed");
    declared.servers.insert("system".into(),SkillServerDecl {raw:serde_json::json!({"command":"/missing/collision"}),source_path:cwd.path().join("mcp.json"),include_tools_by_skill:Default::default()});
    assert_eq!(service.attach_skill_mcp_servers(&declared).await.unwrap().len(),1);
    assert!(!Arc::ptr_eq(&physical,&service.connections["skill"].entry.lock().await.connection));assert_eq!(service.config.as_ref().unwrap().servers["system"].source,McpServerSource::Extension);
    service.dispose().await.unwrap();registry.dispose().await.unwrap();assert_eq!(registry.size(),0);
}
#[tokio::test]
async fn repeated_attach_retains_connections_and_disabled_config_detaches_them() {
    let root=tempfile::tempdir().unwrap();let cwd=tempfile::tempdir().unwrap();let registry=Arc::new(HostMcpRegistry::default());let mut service=McpService::new(registry.clone(),1);
    service.attach_session(cwd.path(),root.path(),&BTreeMap::new(),true,&[]).await.unwrap();assert!(service.server_snapshots().await.is_empty());
    assert!(service.connect_server("missing").await.is_err());assert!(service.reconnect_server("missing").await.is_err());
    let declaration=maho_ext_api::RegisteredMcpServerDeclaration {name:"fixture".into(),config:maho_ext_api::McpServerDeclaration {transport:Some(maho_ext_api::McpTransport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into()]),..Default::default()},extension_path:"fixture-extension".into(),registration_cwd:cwd.path().into()};
    service.attach_session(cwd.path(),root.path(),&BTreeMap::new(),true,std::slice::from_ref(&declaration)).await.unwrap();assert_eq!(registry.size(),1);
    let physical=service.connections["fixture"].entry.lock().await.connection.clone();service.connect_server("fixture").await.unwrap();assert!(!matches!(service.server_snapshots().await[0].lifecycle_state,McpLifecycleState::Other(McpUnspawnedState::NotSpawned)));
    let (_,tools)=service.test_server("fixture").await.unwrap();assert!(tools>0);assert_eq!(service.server_snapshots().await[0].counters.call_count,1);
    let wire=service.wire_status_snapshot().await;assert_eq!(wire.servers.len(),1);assert_eq!(wire.servers[0].name,"fixture");assert!(!wire.servers[0].tools.is_empty());assert!(wire.servers[0].server_info.is_some());
    service.attach_session(cwd.path(),root.path(),&BTreeMap::new(),true,std::slice::from_ref(&declaration)).await.unwrap();assert!(Arc::ptr_eq(&physical,&service.connections["fixture"].entry.lock().await.connection));
    let mut disabled=declaration;disabled.config.enabled=Some(false);service.attach_session(cwd.path(),root.path(),&BTreeMap::new(),true,&[disabled]).await.unwrap();assert_eq!(registry.size(),1);assert!(service.connections.is_empty());
    registry.dispose().await.unwrap();assert_eq!(registry.size(),0);
    service.dispose().await.unwrap();
    let eager=maho_ext_api::RegisteredMcpServerDeclaration {name:"eager".into(),config:maho_ext_api::McpServerDeclaration {transport:Some(maho_ext_api::McpTransport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into()]),lifecycle:Some(maho_ext_api::McpLifecycle::Eager),startup_timeout_ms:Some(0.0),..Default::default()},extension_path:"fixture-extension".into(),registration_cwd:cwd.path().into()};
    service.attach_session(cwd.path(),root.path(),&BTreeMap::new(),true,&[eager]).await.unwrap();
    assert_eq!(service.wait_for_deferred_attach(std::time::Duration::from_secs(15)).await,maho_ext_mcp::startup_race::McpStartupRaceResult::Settled);assert!(service.connections["eager"].entry.lock().await.cached_catalog.is_some());service.dispose().await.unwrap();registry.dispose().await.unwrap();
}
#[tokio::test]
async fn session_instructions_prefer_live_values_and_fall_back_only_when_disconnected() {
    let root=tempfile::tempdir().unwrap();let cwd=tempfile::tempdir().unwrap();let mut service=McpService::new(Arc::new(HostMcpRegistry::default()),1);
    let declaration=maho_ext_api::RegisteredMcpServerDeclaration {name:"instructions".into(),config:maho_ext_api::McpServerDeclaration {transport:Some(maho_ext_api::McpTransport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into(),"--instructions".into(),"cached".into()]),..Default::default()},extension_path:"fixture-extension".into(),registration_cwd:cwd.path().into()};
    service.attach_session(cwd.path(),root.path(),&BTreeMap::new(),true,&[declaration]).await.unwrap();service.connect_server("instructions").await.unwrap();
    let connection=service.connections["instructions"].entry.lock().await.connection.clone();let client=connection.client().unwrap();
    *client.instructions.write().await=Some("live".into());
    let live=maho_ext_mcp::instructions::refresh_mcp_instructions_for_session(&service).await;assert!(live.contains("live"));assert!(!live.contains("cached"));
    *client.instructions.write().await=None;assert!(maho_ext_mcp::instructions::refresh_mcp_instructions_for_session(&service).await.is_empty());
    connection.bump_generation().await.unwrap();assert!(maho_ext_mcp::instructions::refresh_mcp_instructions_for_session(&service).await.contains("cached"));service.dispose().await.unwrap();service.registry.dispose().await.unwrap();
}
#[tokio::test]
async fn two_services_share_the_transport_and_detach_independently() {
    let root=tempfile::tempdir().unwrap();let cwd=tempfile::tempdir().unwrap();let registry=Arc::new(HostMcpRegistry::default());
    let mut first=McpService::new(registry.clone(),1);let mut second=McpService::new(registry.clone(),2);
    let declaration=maho_ext_api::RegisteredMcpServerDeclaration {name:"shared".into(),config:maho_ext_api::McpServerDeclaration {transport:Some(maho_ext_api::McpTransport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into()]),..Default::default()},extension_path:"fixture".into(),registration_cwd:cwd.path().into()};
    first.attach_session(cwd.path(),root.path(),&BTreeMap::new(),true,std::slice::from_ref(&declaration)).await.unwrap();second.attach_session(cwd.path(),root.path(),&BTreeMap::new(),true,std::slice::from_ref(&declaration)).await.unwrap();
    let first_connection=first.connections["shared"].entry.lock().await.connection.clone();let second_connection=second.connections["shared"].entry.lock().await.connection.clone();assert!(Arc::ptr_eq(&first_connection,&second_connection));
    first.connect_server("shared").await.unwrap();let pid=first_connection.get_root_pid().unwrap();first.dispose().await.unwrap();assert!(maho_ext_mcp::process_tree::is_process_alive(pid).await);
    second.connect_server("shared").await.unwrap();assert_eq!(second_connection.get_root_pid(),Some(pid));
    let env=BTreeMap::from([("MCP_TEST_OWNER".into(),"changed".into())]);second.attach_session(cwd.path(),root.path(),&env,true,&[declaration]).await.unwrap();assert!(!Arc::ptr_eq(&second_connection,&second.connections["shared"].entry.lock().await.connection));
    second.dispose().await.unwrap();registry.dispose().await.unwrap();assert!(!maho_ext_mcp::process_tree::is_process_alive(pid).await);
}
