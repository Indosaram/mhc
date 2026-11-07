//! Port of senpi mcp/catalog-cache-adoption.ts: adopt a catalog cached under an earlier, unkeyed identity.
use crate::{
    auth::catalog_identity::legacy_mcp_credential_identity,
    catalog_cache::{
        McpCachedServerCatalog, McpCatalogCacheFile, get_mcp_catalog_cache_path,
        get_valid_cached_server_for, write_mcp_cached_server,
    },
    config_schema::McpServerDeclaredConfig,
};
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCacheIdentity {
    pub config_hash: String,
    pub credential_identity: String,
}

pub struct McpCacheAdoption<'a> {
    pub cache: &'a McpCatalogCacheFile,
    pub agent_dir: Option<&'a Path>,
    pub server_name: &'a str,
    pub current: McpCacheIdentity,
    pub legacy: Box<dyn FnOnce() -> Option<McpCacheIdentity> + 'a>,
}

/// The identities an earlier, unkeyed build persisted for this server, when its credential resolves now.
pub fn legacy_mcp_cache_identity(
    config: &McpServerDeclaredConfig,
    server_name: &str,
    agent_dir: Option<&Path>,
    env: Option<&BTreeMap<String, String>>,
) -> Option<McpCacheIdentity> {
    let credential_identity = legacy_mcp_credential_identity(config, server_name, agent_dir, env)?;
    Some(McpCacheIdentity {
        config_hash: crate::config::legacy_hash_config(config).ok()?,
        credential_identity,
    })
}

/// The cached catalog for the current keyed identity. An entry written under the unkeyed identities is adopted only
/// when both its config and its credential match, so a changed account never inherits another account's catalog,
/// and it is rewritten under the keyed identities.
pub fn adopt_mcp_cached_server(
    adoption: McpCacheAdoption<'_>,
    now: f64,
) -> Option<McpCachedServerCatalog> {
    let McpCacheAdoption {
        cache,
        agent_dir,
        server_name,
        current,
        legacy,
    } = adoption;
    if let Some(cached) = get_valid_cached_server_for(
        cache,
        server_name,
        &current.config_hash,
        Some(&current.credential_identity),
        now,
    ) {
        return Some(cached.clone());
    }
    let legacy = legacy()?;
    let previous = get_valid_cached_server_for(
        cache,
        server_name,
        &legacy.config_hash,
        Some(&legacy.credential_identity),
        now,
    )?;
    let migrated = McpCachedServerCatalog {
        config_hash: current.config_hash,
        credential_identity: Some(current.credential_identity),
        ..previous.clone()
    };
    if let Some(agent_dir) = agent_dir
        && let Err(error) = write_mcp_cached_server(agent_dir, server_name, migrated.clone())
    {
        maho_ext_api::diagnostic(&format!(
            "MCP catalog cache migration write failed for {server_name} at {}: {error}",
            get_mcp_catalog_cache_path(agent_dir).display()
        ));
    }
    Some(migrated)
}
