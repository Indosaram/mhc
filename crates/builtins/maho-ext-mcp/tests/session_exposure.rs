use std::{collections::BTreeMap, time::Duration};
use maho_ext_mcp::{catalog::McpToolCatalogEntry, config_schema::*, expose::session::prepare_mcp_session_catalog};
use serde_json::json;

fn entry(server: &str, tool: &str) -> McpToolCatalogEntry {
    McpToolCatalogEntry {server: server.into(), tool: tool.into(), schema: json!({"type":"object"}), description: None, annotations: None, request_timeout: Duration::from_secs(1), client: None, runtime: None, connection:None, ensure_connected: None, ensure_fresh: None, agent_dir: None, artifacts: None, output_guard: None}
}
fn config() -> ResolvedMcpConfig {
    ResolvedMcpConfig {settings: default_settings(), diagnostics: vec![], servers: BTreeMap::new()}
}
fn server(name: &str, exposure: Exposure) -> ResolvedMcpServer {
    ResolvedMcpServer {name: name.into(), source: McpServerSource::Global, source_path: "mcp.json".into(), state: McpServerState::Enabled, transport: Some(Transport::Stdio), config_hash: None, config: Some(McpServerConfig {exposure: Some(exposure), ..Default::default()})}
}
#[test]
fn empty_catalog_preserves_active_set_unless_refresh_requested() {
    assert!(prepare_mcp_session_catalog(&config(), [], false).is_none());
    assert!(prepare_mcp_session_catalog(&config(), [], true).is_some());
}
#[test]
fn mixed_modes_keep_proxy_catalog_separate_and_only_direct_tools_active() {
    let mut config = config();
    for (name, mode) in [("direct", Exposure::Direct), ("search", Exposure::Search), ("proxy", Exposure::Proxy)] {config.servers.insert(name.into(), server(name, mode));}
    let prepared = prepare_mcp_session_catalog(&config, [
        ("direct".into(), vec![entry("direct", "a")]),
        ("search".into(), vec![entry("search", "b")]),
        ("proxy".into(), vec![entry("proxy", "c")]),
        ("missing".into(), vec![entry("missing", "d")]),
    ], false).unwrap();
    assert!(prepared.search_mode); assert_eq!(prepared.registered_entries.len(), 2);
    assert_eq!(prepared.active_entries.len(), 1); assert_eq!(prepared.active_entries[0].server, "direct");
    assert_eq!(prepared.proxy_gateways[0].0, "proxy"); assert_eq!(prepared.proxy_gateways[0].1[0].tool, "c");
}
#[test]
fn filtered_empty_server_warns_and_does_not_trigger_registration() {
    let mut config = config(); let mut resolved = server("fx", Exposure::Search);
    resolved.config.as_mut().unwrap().include_tools = Some(vec!["missing_*".into()]);
    config.servers.insert("fx".into(), resolved);
    assert!(prepare_mcp_session_catalog(&config, [("fx".into(), vec![entry("fx", "a")])], false).is_none());
    let refreshed = prepare_mcp_session_catalog(&config, [("fx".into(), vec![entry("fx", "a")])], true).unwrap();
    assert_eq!(refreshed.warnings.len(), 1); assert_eq!(refreshed.warnings[0].0, "fx");
}
