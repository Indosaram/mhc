use std::sync::{Arc, Mutex};
use crate::{catalog::McpCatalogCallback, catalog_cache::McpCachedServerCatalog, config_schema::ResolvedMcpConfig, connection::ServerConnection, log::McpLogger, service_types::McpConnectionEntry};

pub struct McpServiceRegistrationEntry {
    pub name: String,
    pub connection: Arc<ServerConnection>,
    pub logger: Arc<Mutex<McpLogger>>,
    pub agent_dir: Option<std::path::PathBuf>,
    pub cached_catalog: Option<McpCachedServerCatalog>,
    pub artifacts: Option<Arc<crate::guard::output_guard::McpOutputArtifacts>>,
    pub ensure_fresh: McpCatalogCallback,
    pub ensure_cached_tool_connected: McpCatalogCallback,
}

pub async fn prepare_mcp_service_registration_entries(
    config: &ResolvedMcpConfig,
    entries: &[Arc<tokio::sync::Mutex<McpConnectionEntry>>],
) -> Vec<McpServiceRegistrationEntry> {
    let mut prepared = Vec::with_capacity(entries.len());
    for entry in entries {
        let snapshot = entry.lock().await;
        let server_config = config.servers.get(&snapshot.name).and_then(|server| server.config.clone());
        let refresh_entry = entry.clone();
        let auth_entry = entry.clone();
        prepared.push(McpServiceRegistrationEntry {
            name: snapshot.name.clone(), connection: snapshot.connection.clone(), logger: snapshot.logger.clone(),
            agent_dir: snapshot.agent_dir.clone(), cached_catalog: snapshot.cached_catalog.clone(),
            artifacts: snapshot.artifacts.clone(),
            ensure_fresh: Arc::new(move || {
                let entry = auth_entry.clone();
                Box::pin(async move {
                    entry.lock().await.auth_plan.ensure_fresh().await
                })
            }),
            ensure_cached_tool_connected: Arc::new(move || {
                let entry = refresh_entry.clone(); let config = server_config.clone();
                Box::pin(async move {
                    if let Some(config) = config {crate::startup_race::connect_and_refresh_mcp_catalog(&mut *entry.lock().await, &config).await?;}
                    Ok(())
                })
            }),
        });
    }
    prepared
}

pub async fn register_mcp_service_direct_tools(
    registrar: std::sync::Arc<dyn crate::tool_registrar::McpToolRegistrar>,
    config: &ResolvedMcpConfig,
    entries: &[std::sync::Arc<tokio::sync::Mutex<McpConnectionEntry>>],
    tool_search: Option<&mut maho_ext_tool_search::service::ToolSearchService>,
    registry: &mut crate::expose::tier_b::McpTierBRegistry,
    refresh_active_set_when_empty: bool,
) -> Result<Option<crate::expose::session::McpSessionRegistration>, maho_ext_api::ExtensionFailure> {
    let prepared = prepare_mcp_service_registration_entries(config, entries).await;
    crate::expose::session::register_direct_mcp_tools(registrar, config, prepared, tool_search, registry, refresh_active_set_when_empty).await
}
