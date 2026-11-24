use maho_ext_api::{ExtensionFailure, ToolDefinition};
use maho_ext_mcp::{
    catalog::McpToolCatalogEntry,
    catalog_cache::McpCachedServerCatalog,
    config_schema::*,
    connection::ServerConnection,
    expose::session::{prepare_mcp_session_catalog, register_direct_mcp_tools},
    log::McpLogger,
    service_register::McpServiceRegistrationEntry,
    tool_registrar::McpToolRegistrar,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

fn entry(server: &str, tool: &str) -> McpToolCatalogEntry {
    McpToolCatalogEntry {
        server: server.into(),
        tool: tool.into(),
        schema: json!({"type":"object"}),
        output_schema: None,
        modern: false,
        description: None,
        annotations: None,
        request_timeout: Duration::from_secs(1),
        client: None,
        runtime: None,
        connection: None,
        ensure_connected: None,
        ensure_fresh: None,
        agent_dir: None,
        artifacts: None,
        output_guard: None,
        request_origin: None,
        invocation: None,
    }
}
fn config() -> ResolvedMcpConfig {
    ResolvedMcpConfig {
        settings: default_settings(),
        diagnostics: vec![],
        servers: BTreeMap::new(),
    }
}
fn server(name: &str, exposure: Exposure) -> ResolvedMcpServer {
    ResolvedMcpServer {
        name: name.into(),
        source: McpServerSource::Global,
        source_path: "mcp.json".into(),
        state: McpServerState::Enabled,
        transport: Some(Transport::Stdio),
        config_hash: None,
        config: Some(McpServerConfig {
            exposure: Some(exposure),
            ..Default::default()
        }),
    }
}
#[test]
fn empty_catalog_preserves_active_set_unless_refresh_requested() {
    assert!(prepare_mcp_session_catalog(&config(), [], false).is_none());
    assert!(prepare_mcp_session_catalog(&config(), [], true).is_some());
}
#[test]
fn mixed_modes_keep_proxy_catalog_separate_and_only_direct_tools_active() {
    let mut config = config();
    for (name, mode) in [
        ("direct", Exposure::Direct),
        ("search", Exposure::Search),
        ("proxy", Exposure::Proxy),
    ] {
        config.servers.insert(name.into(), server(name, mode));
    }
    let prepared = prepare_mcp_session_catalog(
        &config,
        [
            ("direct".into(), vec![entry("direct", "a")]),
            ("search".into(), vec![entry("search", "b")]),
            ("proxy".into(), vec![entry("proxy", "c")]),
            ("missing".into(), vec![entry("missing", "d")]),
        ],
        false,
    )
    .unwrap();
    assert!(prepared.search_mode);
    assert_eq!(prepared.registered_entries.len(), 2);
    assert_eq!(prepared.active_entries.len(), 1);
    assert_eq!(prepared.active_entries[0].server, "direct");
    assert_eq!(prepared.proxy_gateways[0].0, "proxy");
    assert_eq!(prepared.proxy_gateways[0].1[0].tool, "c");
}
#[test]
fn filtered_empty_server_warns_and_does_not_trigger_registration() {
    let mut config = config();
    let mut resolved = server("fx", Exposure::Search);
    resolved.config.as_mut().unwrap().include_tools = Some(vec!["missing_*".into()]);
    config.servers.insert("fx".into(), resolved);
    assert!(
        prepare_mcp_session_catalog(&config, [("fx".into(), vec![entry("fx", "a")])], false)
            .is_none()
    );
    let refreshed =
        prepare_mcp_session_catalog(&config, [("fx".into(), vec![entry("fx", "a")])], true)
            .unwrap();
    assert_eq!(refreshed.warnings.len(), 1);
    assert_eq!(refreshed.warnings[0].0, "fx");
}

struct TestRegistrar;
impl McpToolRegistrar for TestRegistrar {
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> {
        Ok(Vec::new())
    }
    fn set_active_tools(&self, _: Vec<String>) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn register_tool(&self, _: ToolDefinition) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn register_tool_with_renderers(
        &self,
        _: ToolDefinition,
        _: maho_ext_mcp::tool_registrar::McpToolRenderers,
    ) -> Result<(), ExtensionFailure> {
        Ok(())
    }
}

fn registration_entry(
    name: &str,
    root: &std::path::Path,
    cached_catalog: Option<McpCachedServerCatalog>,
) -> McpServiceRegistrationEntry {
    let config = McpServerConfig {
        enabled: Some(true),
        ..Default::default()
    };
    let logger = Arc::new(Mutex::new(McpLogger::new(name, root, None).unwrap()));
    McpServiceRegistrationEntry {
        name: name.into(),
        connection: ServerConnection::new(name, config, None, logger.clone()),
        logger,
        agent_dir: None,
        cached_catalog,
        artifacts: None,
        ensure_fresh: Arc::new(|| Box::pin(async { Ok(()) })),
        ensure_cached_tool_connected: Arc::new(|| Box::pin(async { Ok(()) })),
        request_origin: None,
        bind_presentation: None,
        invocation: None,
    }
}

#[tokio::test]
async fn unconnected_server_without_cache_does_not_log_filter_warning() {
    let root = tempfile::tempdir().unwrap();
    let entry = registration_entry("fx", root.path(), None);
    let logger = entry.logger.clone();
    let mut config = config();
    config
        .servers
        .insert("fx".into(), server("fx", Exposure::Direct));
    let mut registry = maho_ext_mcp::expose::tier_b::McpTierBRegistry::default();
    let result = register_direct_mcp_tools(
        Arc::new(TestRegistrar),
        &config,
        vec![entry],
        None,
        &mut registry,
        false,
    )
    .await
    .unwrap();
    assert!(result.is_none());
    let lines = logger.lock().unwrap().get_ring_buffer();
    assert!(
        lines
            .iter()
            .all(|line| { !line.contains("<unknown>") && !line.contains("0 exposed tools") })
    );
}

#[tokio::test]
async fn cached_server_filtered_to_zero_still_logs_server_name_warning() {
    let root = tempfile::tempdir().unwrap();
    let cached = McpCachedServerCatalog {
        modern: false,
        config_hash: "hash".into(),
        credential_identity: None,
        fetched_at: 0.0,
        tools: vec![json!({"name":"present", "inputSchema":{"type":"object"}})],
        resources: vec![],
        prompts: vec![],
        instructions: None,
    };
    let entry = registration_entry("fx", root.path(), Some(cached));
    let logger = entry.logger.clone();
    let mut config = config();
    let mut resolved = server("fx", Exposure::Direct);
    resolved.config.as_mut().unwrap().include_tools = Some(vec!["missing_*".into()]);
    config.servers.insert("fx".into(), resolved);
    let mut registry = maho_ext_mcp::expose::tier_b::McpTierBRegistry::default();
    let _ = register_direct_mcp_tools(
        Arc::new(TestRegistrar),
        &config,
        vec![entry],
        None,
        &mut registry,
        false,
    )
    .await
    .unwrap();
    let lines = logger.lock().unwrap().get_ring_buffer();
    assert!(lines.iter().any(|line| {
        line.contains("MCP server fx has 0 exposed tools after includeTools/excludeTools filters.")
    }));
}
