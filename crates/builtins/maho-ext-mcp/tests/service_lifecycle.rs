//! Startup-race clause and `list_changed` re-registration lifecycle.
//!
//! Covers the two pinned registration triggers the native port had not wired:
//! `syncFromConfig`'s `cachedCatalog === undefined` race clause and the coalesced
//! `#handleServerToolsChanged` re-registration from `#wireListChanged`.

use std::sync::{Arc, Mutex};

use maho_ext_api::{ExtensionFailure, ToolDefinition};
use maho_ext_mcp::connection::ServerConnectionState;
use maho_ext_mcp::host_registry::HostMcpRegistry;
use maho_ext_mcp::service::McpService;
use maho_ext_mcp::startup_race::McpStartupRaceResult;
use maho_ext_mcp::tool_registrar::McpToolRegistrar;

const FIXTURE: &str = "/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts";

#[derive(Default)]
struct RecordingRegistrar {
    registered: Mutex<Vec<String>>,
    active: Mutex<Vec<String>>,
}
impl McpToolRegistrar for RecordingRegistrar {
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> {
        Ok(self.active.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone())
    }
    fn set_active_tools(&self, names: Vec<String>) -> Result<(), ExtensionFailure> {
        *self.active.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = names;
        Ok(())
    }
    fn register_tool(&self, definition: ToolDefinition) -> Result<(), ExtensionFailure> {
        self.registered.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(definition.name);
        Ok(())
    }
}

fn declaration(name: &str, cwd: &std::path::Path, extra_args: &[&str]) -> maho_ext_api::RegisteredMcpServerDeclaration {
    let mut args = vec![FIXTURE.to_owned()];
    args.extend(extra_args.iter().map(|arg| (*arg).to_owned()));
    maho_ext_api::RegisteredMcpServerDeclaration {
        name: name.into(),
        config: maho_ext_api::McpServerDeclaration {
            transport: Some(maho_ext_api::McpTransport::Stdio),
            command: Some("/usr/bin/node".into()),
            args: Some(args),
            exposure: Some(maho_ext_api::McpExposure::Direct),
            ..Default::default()
        },
        extension_path: "fixture".into(),
        registration_cwd: cwd.into(),
    }
}

/// Seed a valid on-disk catalog so the lazy server is cached and does not race at attach
/// (upstream: `shouldRaceMcpStartup(lifecycle) || cachedCatalog === undefined`).
fn seed_cache(agent_dir: &std::path::Path, name: &str, declaration: &maho_ext_api::RegisteredMcpServerDeclaration) {
    let mut wire = maho_ext_mcp::config_schema::ServerConfigWire::from(&declaration.config);
    wire.cwd.get_or_insert_with(|| declaration.registration_cwd.to_string_lossy().into_owned());
    let hash = maho_ext_mcp::config::hash_config(&maho_ext_mcp::config::normalize_server(wire)).expect("hash the seeded server config");
    maho_ext_mcp::catalog_cache::write_mcp_cached_server(agent_dir, name, maho_ext_mcp::catalog_cache::McpCachedServerCatalog {config_hash: hash, fetched_at: chrono::Utc::now().timestamp_millis() as f64, tools: vec![serde_json::json!({"name":"tool_1","inputSchema":{"type":"object"}})], resources: vec![], prompts: vec![], instructions: None}).expect("write the seeded mcp catalog");
}

#[tokio::test]
async fn lazy_server_without_a_cache_races_at_attach() {
    let root = tempfile::tempdir().unwrap();
    let registry = Arc::new(HostMcpRegistry::default());
    let mut service = McpService::new(registry.clone(), 1);
    let declaration = declaration("fx", root.path(), &[]);
    service.attach_session(root.path(), root.path(), &Default::default(), true, &[declaration]).await.unwrap();
    assert_eq!(service.wait_for_deferred_attach(std::time::Duration::from_secs(15)).await, McpStartupRaceResult::Settled);
    let entry = service.connections["fx"].entry.clone();
    assert_eq!(entry.lock().await.connection.state(), ServerConnectionState::Connected);
    assert!(entry.lock().await.cached_catalog.is_some());
    service.dispose().await.unwrap();
    registry.dispose().await.unwrap();
}

#[tokio::test]
async fn lazy_server_with_a_valid_cache_skips_the_startup_race() {
    let root = tempfile::tempdir().unwrap();
    let registry = Arc::new(HostMcpRegistry::default());
    let mut service = McpService::new(registry.clone(), 1);
    let declaration = declaration("fx", root.path(), &[]);
    seed_cache(root.path(), "fx", &declaration);
    service.attach_session(root.path(), root.path(), &Default::default(), true, &[declaration]).await.unwrap();
    let entry = service.connections["fx"].entry.clone();
    let connection = entry.lock().await.connection.clone();
    assert_eq!(connection.state(), ServerConnectionState::Idle);
    assert!(entry.lock().await.cached_catalog.is_some());
    assert!(connection.get_root_pid().is_none(), "a cached lazy server must not spawn a process at attach");
    service.dispose().await.unwrap();
    registry.dispose().await.unwrap();
}

#[tokio::test]
async fn list_changed_reregisters_and_records_the_delta() {
    let root = tempfile::tempdir().unwrap();
    let registry = Arc::new(HostMcpRegistry::default());
    let handle = Arc::new(tokio::sync::Mutex::new(McpService::new(registry.clone(), 1)));
    let registrar: Arc<dyn McpToolRegistrar> = Arc::new(RecordingRegistrar::default());
    {
        let mut service = handle.lock().await;
        service.bind_self(Arc::downgrade(&handle));
        service.bind_registration(registrar.clone(), None);
    }
    let declaration = declaration("fx", root.path(), &["--tools", "2"]);
    handle.lock().await.attach_session(root.path(), root.path(), &Default::default(), true, &[declaration]).await.unwrap();
    assert_eq!(handle.lock().await.wait_for_deferred_attach(std::time::Duration::from_secs(15)).await, McpStartupRaceResult::Settled);
    let entry = handle.lock().await.connections["fx"].entry.clone();
    {
        let entry = entry.lock().await;
        assert_eq!(entry.list_changed_tasks.len(), 1);
        assert!(entry.list_changed_coalescer.is_some());
    }
    assert_eq!(entry.lock().await.connection.state(), ServerConnectionState::Connected);
    handle.lock().await.handle_server_tools_changed("fx").await.unwrap();
    let guard = entry.lock().await;
    assert_eq!(guard.last_list_changed_delta.as_deref(), Some("no change"));
    assert_eq!(guard.known_tool_names.as_ref().map(Vec::len), Some(2));
    drop(guard);
    handle.lock().await.dispose().await.unwrap();
    assert!(entry.lock().await.list_changed_coalescer.is_none());
    assert!(entry.lock().await.list_changed_tasks.is_empty());
    registry.dispose().await.unwrap();
}
