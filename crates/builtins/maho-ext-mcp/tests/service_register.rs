use std::{collections::BTreeMap, sync::Arc};
use maho_ext_mcp::{host_registry::HostMcpRegistry, service::McpService, service_register::prepare_mcp_service_registration_entries};

/// Seed a valid on-disk catalog for `name` so the lazy server is cached and does
/// not race at attach (upstream: `shouldRaceMcpStartup(lifecycle) || cachedCatalog === undefined`).
fn seed_cache(agent_dir: &std::path::Path, name: &str, declaration: &maho_ext_api::RegisteredMcpServerDeclaration) {
    let mut wire = maho_ext_mcp::config_schema::ServerConfigWire::from(&declaration.config);
    wire.cwd.get_or_insert_with(|| declaration.registration_cwd.to_string_lossy().into_owned());
    let hash = maho_ext_mcp::config::hash_config(&maho_ext_mcp::config::normalize_server(wire)).unwrap();
    maho_ext_mcp::catalog_cache::write_mcp_cached_server(agent_dir, name, maho_ext_mcp::catalog_cache::McpCachedServerCatalog {config_hash: hash, fetched_at: chrono::Utc::now().timestamp_millis() as f64, tools: vec![serde_json::json!({"name":"tool_1","inputSchema":{"type":"object"}})], resources: vec![], prompts: vec![], instructions: None}).unwrap();
}

#[tokio::test]
async fn service_registration_snapshot_refreshes_the_original_entry_catalog() {
    let root = tempfile::tempdir().unwrap(); let cwd = tempfile::tempdir().unwrap();
    let registry = Arc::new(HostMcpRegistry::default()); let mut service = McpService::new(registry.clone(), 1);
    let declaration = maho_ext_api::RegisteredMcpServerDeclaration {name:"fx".into(), config:maho_ext_api::McpServerDeclaration {transport:Some(maho_ext_api::McpTransport::Stdio), command:Some("/usr/bin/node".into()), args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into()]), ..Default::default()}, extension_path:"fixture".into(), registration_cwd:cwd.path().into()};
    seed_cache(root.path(), "fx", &declaration);
    service.attach_session(cwd.path(), root.path(), &BTreeMap::new(), true, &[declaration]).await.unwrap();
    let original = service.connections["fx"].entry.clone();
    let prepared = prepare_mcp_service_registration_entries(service.config.as_ref().unwrap(), std::slice::from_ref(&original)).await;
    assert_eq!(prepared[0].name, "fx"); assert!(prepared[0].cached_catalog.is_some());
    assert!(Arc::ptr_eq(&prepared[0].connection, &original.lock().await.connection));
    (prepared[0].ensure_cached_tool_connected)().await.unwrap();
    assert!(!original.lock().await.cached_catalog.as_ref().unwrap().tools.is_empty());
    assert!(prepared[0].cached_catalog.is_some());
    service.dispose().await.unwrap(); registry.dispose().await.unwrap();
}

#[tokio::test]
async fn service_registration_preserves_artifact_owner_and_optional_auth_refresh() {
    // Given a connected-session entry with a session-owned output artifact tracker.
    let root = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let registry = Arc::new(HostMcpRegistry::default());
    let mut service = McpService::new(registry.clone(), 1);
    let declaration = maho_ext_api::RegisteredMcpServerDeclaration {
        name: "fx".into(),
        config: maho_ext_api::McpServerDeclaration {
            transport: Some(maho_ext_api::McpTransport::Stdio),
            command: Some("/usr/bin/node".into()),
            ..Default::default()
        },
        extension_path: "fixture".into(),
        registration_cwd: cwd.path().into(),
    };
    seed_cache(root.path(), "fx", &declaration);
    service.attach_session(cwd.path(), root.path(), &BTreeMap::new(), true, &[declaration]).await.unwrap();
    let original = service.connections["fx"].entry.clone();
    let artifacts = original.lock().await.artifacts.as_ref().unwrap().clone();
    let spill = root.path().join("owned-spill.txt");
    std::fs::write(&spill, "output").unwrap();
    artifacts.track(spill.clone());

    // When the entry is adapted for exposure registration.
    let prepared = prepare_mcp_service_registration_entries(
        service.config.as_ref().unwrap(), std::slice::from_ref(&original),
    ).await;

    // Then registration retains the same tracker, and non-OAuth refresh is a no-op.
    assert!(Arc::ptr_eq(prepared[0].artifacts.as_ref().unwrap(), &artifacts));
    (prepared[0].ensure_fresh)().await.unwrap();
    assert_eq!(original.lock().await.connection.state(), maho_ext_mcp::connection::ServerConnectionState::Idle);
    service.dispose().await.unwrap();
    assert!(!spill.exists());
    registry.dispose().await.unwrap();
}
