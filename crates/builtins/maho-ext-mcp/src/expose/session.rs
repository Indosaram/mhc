use crate::{catalog::McpToolCatalogEntry, config_schema::{Exposure, ResolvedMcpConfig}, expose::policy::compute_mcp_exposure_policy};

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
