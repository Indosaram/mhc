//! Session catalog preparation and direct/Tier-B tool registration.
use std::sync::Arc;
use std::time::Duration;
use maho_ext_api::ExtensionFailure;
use maho_ext_tool_search::service::ToolSearchService;
use serde_json::Value;

use crate::catalog::McpToolCatalogEntry;
use crate::config_schema::{Exposure,McpServerConfig,McpSettings,ResolvedMcpConfig};
use crate::connection::ServerConnectionState;
use crate::expose::policy::compute_mcp_exposure_policy;
use crate::guard::output_guard::McpOutputArtifacts;
use crate::prompts::McpPromptServer;
use crate::resources::{create_mcp_resource_tools,McpResourceServer};
use crate::service_register::McpServiceRegistrationEntry;
use crate::tool_registrar::McpToolRegistrar;
use super::tier_b::{register_mcp_tier_b_tools,McpTierBRegistration,McpTierBRegistrationInput,McpTierBRegistry};

#[derive(Default)]
pub struct McpSessionCatalog {
    pub registered_entries: Vec<McpToolCatalogEntry>,
    pub active_entries: Vec<McpToolCatalogEntry>,
    pub search_mode: bool,
    pub proxy_gateways: Vec<(String, Vec<McpToolCatalogEntry>)>,
    pub warnings: Vec<(String, String)>,
}

pub fn prepare_mcp_session_catalog(
    config: &ResolvedMcpConfig,
    catalogs: impl IntoIterator<Item = (String, Vec<McpToolCatalogEntry>)>,
    refresh_active_set_when_empty: bool,
) -> Option<McpSessionCatalog> {
    let mut prepared = McpSessionCatalog::default();
    for (name, catalog) in catalogs {
        let Some(server) = config.servers.get(&name).and_then(|server| server.config.as_ref()) else {continue;};
        let policy = compute_mcp_exposure_policy(&catalog, server, &config.settings);
        prepared.warnings.extend(policy.warnings.into_iter().map(|warning| (name.clone(), warning)));
        if policy.mode == Exposure::Search {prepared.search_mode = true;}
        if policy.mode == Exposure::Proxy {prepared.proxy_gateways.push((name, policy.filtered_entries));}
        prepared.registered_entries.extend(policy.registered_entries);
        prepared.active_entries.extend(policy.active_entries);
    }
    if prepared.registered_entries.is_empty() && prepared.active_entries.is_empty()
        && !prepared.search_mode && prepared.proxy_gateways.is_empty() && !refresh_active_set_when_empty {
        None
    } else {Some(prepared)}
}

pub struct McpSessionRegistration {
    pub registration: McpTierBRegistration,
    pub prompt_servers: Vec<McpPromptServer>,
    pub resource_servers: Vec<McpResourceServer>,
    pub registered_tools: Vec<String>,
    pub wiring_errors: Vec<String>,
}

fn warn(entry: &McpServiceRegistrationEntry, message: &str) {
    let _ = entry.logger.lock().unwrap_or_else(std::sync::PoisonError::into_inner).log("warning", message, None, None);
}

fn request_timeout(server: &McpServerConfig) -> Duration {
    Duration::from_secs_f64(server.request_timeout_ms.unwrap_or(30000.0) / 1000.0)
}

fn catalog_entry(entry: &McpServiceRegistrationEntry, _server: &McpServerConfig, settings: &McpSettings, tool: &Value, timeout: Duration, client: Option<Arc<crate::transport_sdk::McpClient>>) -> McpToolCatalogEntry {
    McpToolCatalogEntry {
        server: entry.name.clone(), tool: tool.get("name").and_then(Value::as_str).unwrap_or("").into(),
        schema: tool.get("inputSchema").cloned().unwrap_or(Value::Null), description: tool.get("description").and_then(Value::as_str).map(str::to_owned),
        annotations: tool.get("annotations").cloned(), request_timeout: timeout, client,
        runtime: None, connection:Some(entry.connection.clone()), ensure_connected: Some(entry.ensure_cached_tool_connected.clone()), ensure_fresh: Some(entry.ensure_fresh.clone()),
        agent_dir: entry.agent_dir.clone(), artifacts: entry.artifacts.clone(), output_guard: settings.output_guard.clone(),
    }
}

async fn build_catalog(entry: &McpServiceRegistrationEntry, server: &McpServerConfig, settings: &McpSettings) -> Vec<McpToolCatalogEntry> {
    let timeout = request_timeout(server);
    if let Some(cached) = &entry.cached_catalog {
        let client = entry.connection.client().ok();
        return cached.tools.iter().map(|tool| catalog_entry(entry, server, settings, tool, timeout, client.clone())).collect();
    }
    if entry.connection.state() == ServerConnectionState::Connected
        && let Ok(client) = entry.connection.client()
        && let Ok(collected) = crate::catalog::collect_tool_catalog(&entry.name, client, timeout).await
    {
        return collected.into_iter().map(|mut collected| {
            collected.ensure_fresh = Some(entry.ensure_fresh.clone());
            collected.connection=Some(entry.connection.clone());
            collected.ensure_connected = Some(entry.ensure_cached_tool_connected.clone());
            collected.agent_dir = entry.agent_dir.clone();
            collected.artifacts = entry.artifacts.clone();
            collected.output_guard = settings.output_guard.clone();
            collected
        }).collect();
    }
    Vec::new()
}

fn resource_server(entry: &McpServiceRegistrationEntry, server: &McpServerConfig, settings: &McpSettings) -> Option<McpResourceServer> {
    let cached = entry.cached_catalog.as_ref()?;
    if cached.resources.is_empty() {return None;}
    let client = entry.connection.client().ok()?;
    Some(McpResourceServer {
        server: entry.name.clone(), client, agent_dir: entry.agent_dir.clone().unwrap_or_default(),
        artifacts: entry.artifacts.clone().unwrap_or_else(|| Arc::new(McpOutputArtifacts::default())),
        output_guard: settings.output_guard.clone(), request_timeout: request_timeout(server), resources: cached.resources.clone(),
    })
}

pub async fn register_direct_mcp_tools(
    registrar: Arc<dyn McpToolRegistrar>,
    config: &ResolvedMcpConfig,
    entries: Vec<McpServiceRegistrationEntry>,
    mut tool_search: Option<&mut ToolSearchService>,
    registry: &mut McpTierBRegistry,
    refresh_active_set_when_empty: bool,
) -> Result<Option<McpSessionRegistration>, ExtensionFailure> {
    let mut registered_entries: Vec<McpToolCatalogEntry> = Vec::new();
    let mut active_entries: Vec<McpToolCatalogEntry> = Vec::new();
    let mut search_mode = false;
    let mut proxy_gateways: Vec<(String, Vec<McpToolCatalogEntry>)> = Vec::new();
    let mut resource_servers: Vec<McpResourceServer> = Vec::new();
    let mut prompt_servers: Vec<McpPromptServer> = Vec::new();
    let mut wiring_errors: Vec<String> = Vec::new();
    for entry in &entries {
        let Some(server) = config.servers.get(&entry.name).and_then(|server| server.config.as_ref()) else {continue;};
        let catalog = build_catalog(entry, server, &config.settings).await;
        let policy = compute_mcp_exposure_policy(&catalog, server, &config.settings);
        if policy.mode == Exposure::Search && tool_search.is_none() {
            wiring_errors.push(format!("MCP server {} resolved to search exposure but the shared tool_search service is unwired; its {} tool(s) were not registered. Host tool-search wiring must pass the service to register_mcp_lifecycle.", entry.name, policy.registered_entries.len()));
            continue;
        }
        for warning in &policy.warnings {warn(entry, warning);}
        if let Some(cached) = &entry.cached_catalog && !cached.prompts.is_empty() {
            prompt_servers.push(McpPromptServer {server: entry.name.clone(), connection: entry.connection.clone(), request_timeout: request_timeout(server), prompts: cached.prompts.clone()});
        }
        if let Some(resources) = resource_server(entry, server, &config.settings) {resource_servers.push(resources);}
        if policy.mode == Exposure::Search {search_mode = true;}
        if policy.mode == Exposure::Proxy {proxy_gateways.push((entry.name.clone(), policy.filtered_entries.clone()));}
        registered_entries.extend(policy.registered_entries);
        active_entries.extend(policy.active_entries);
    }
    if registered_entries.is_empty() && active_entries.is_empty() && !search_mode && proxy_gateways.is_empty() && wiring_errors.is_empty() && !refresh_active_set_when_empty {
        return Ok(None);
    }
    if registered_entries.is_empty() && active_entries.is_empty() && !search_mode && proxy_gateways.is_empty() && !refresh_active_set_when_empty {
        return Ok(Some(McpSessionRegistration {registration: McpTierBRegistration {searchable: Vec::new(), activate: {
            let activate: maho_ext_tool_search::service::FeederActivate = Arc::new(|_: &[String]| Ok(()));
            activate
        }}, prompt_servers, resource_servers, registered_tools: Vec::new(), wiring_errors}));
    }
    let utility_tools = if resource_servers.is_empty() {Vec::new()} else {
        let servers = resource_servers.clone();
        create_mcp_resource_tools(Arc::new(move || servers.clone()))
    };
    let agent_dir = entries.first().and_then(|entry| entry.agent_dir.clone()).unwrap_or_default();
    let artifacts = entries.first().and_then(|entry| entry.artifacts.clone()).unwrap_or_else(|| Arc::new(McpOutputArtifacts::default()));
    let registration = register_mcp_tier_b_tools(registrar, McpTierBRegistrationInput {
        registered_entries, active_entries, search_mode, proxy_gateways, utility_tools,
        settings: config.settings.clone(), agent_dir, artifacts, output_guard: config.settings.output_guard.clone(),
    }, tool_search.as_deref_mut(), registry, None)?;
    let registered_tools = registration.searchable.iter().map(|tool| tool.name.clone()).collect();
    Ok(Some(McpSessionRegistration {registration, prompt_servers, resource_servers, registered_tools, wiring_errors}))
}
