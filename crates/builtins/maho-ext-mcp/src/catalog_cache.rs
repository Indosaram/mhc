use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCachedServerCatalog {
    /// Collected from a modern (2026-07-28) server: tools keep every field and may carry boolean schemas.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub modern: bool,
    pub config_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_identity: Option<String>,
    pub fetched_at: f64,
    pub tools: Vec<Value>,
    pub resources: Vec<Value>,
    pub prompts: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpCatalogCacheFile {
    pub version: u8,
    pub servers: BTreeMap<String, McpCachedServerCatalog>,
}
impl Default for McpCatalogCacheFile {
    fn default() -> Self {
        Self {
            version: 1,
            servers: BTreeMap::new(),
        }
    }
}
pub fn get_mcp_catalog_cache_path(agent_dir: &Path) -> PathBuf {
    agent_dir.join("cache/mcp-cache.json")
}
pub fn read_mcp_catalog_cache(agent_dir: &Path) -> McpCatalogCacheFile {
    match std::fs::read_to_string(get_mcp_catalog_cache_path(agent_dir))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    {
        Some(value) => normalize_cache_file(&value),
        None => McpCatalogCacheFile::default(),
    }
}
pub fn normalize_cache_file(value: &Value) -> McpCatalogCacheFile {
    if value.get("version") != Some(&json!(1)) {
        return McpCatalogCacheFile::default();
    }
    let Some(servers) = value.get("servers").and_then(Value::as_object) else {
        return McpCatalogCacheFile::default();
    };
    let mut cache = McpCatalogCacheFile::default();
    for (name, server) in servers {
        if let Some(catalog) = normalize_cached_server(server) {
            cache.servers.insert(name.clone(), catalog);
        }
    }
    cache
}
fn normalize_cached_server(value: &Value) -> Option<McpCachedServerCatalog> {
    let config_hash = value.get("configHash")?.as_str()?.to_owned();
    let fetched_at = value.get("fetchedAt")?.as_f64()?;
    let modern = value.get("modern") == Some(&Value::Bool(true));
    let mut tools = Vec::new();
    for tool in value.get("tools")?.as_array()? {
        let name = tool.get("name")?.as_str()?;
        let schema = tool.get("inputSchema")?;
        if !(schema.is_object() || modern && schema.is_boolean()) {
            return None;
        }
        // A modern tool keeps every field it was listed with; a legacy one only the known ones.
        let mut normalized = if modern { tool.clone() } else { json!({}) };
        normalized["name"] = json!(name);
        normalized["inputSchema"] = schema.clone();
        match tool.get("description").filter(|v| v.is_string()) {
            Some(description) => normalized["description"] = description.clone(),
            None => {
                if let Some(object) = normalized.as_object_mut() {
                    object.remove("description");
                }
            }
        }
        match tool.get("annotations").filter(|v| v.is_object()) {
            Some(annotations) => normalized["annotations"] = annotations.clone(),
            None => {
                if let Some(object) = normalized.as_object_mut() {
                    object.remove("annotations");
                }
            }
        }
        if let Some(object) = normalized.as_object_mut()
            && !(modern
                && tool
                    .get("outputSchema")
                    .is_some_and(|output| output.is_object() || output.is_boolean()))
        {
            object.remove("outputSchema");
        }
        if !modern || crate::modern::tool_schema::has_supported_modern_tool_schemas(&normalized) {
            tools.push(normalized);
        }
    }
    Some(McpCachedServerCatalog {
        modern,
        config_hash,
        credential_identity: value
            .get("credentialIdentity")
            .and_then(Value::as_str)
            .map(str::to_owned),
        fetched_at,
        tools,
        resources: value
            .get("resources")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        prompts: value
            .get("prompts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        instructions: value
            .get("instructions")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}
pub fn get_valid_cached_server<'a>(
    cache: &'a McpCatalogCacheFile,
    name: &str,
    hash: &str,
    now: f64,
) -> Option<&'a McpCachedServerCatalog> {
    cache
        .servers
        .get(name)
        .filter(|s| s.config_hash == hash && now - s.fetched_at <= 604800000.0)
}
/// `get_valid_cached_server` bound to a credential identity: `None` matches an entry stored without one
/// (upstream's `"none"` default).
pub fn get_valid_cached_server_for<'a>(
    cache: &'a McpCatalogCacheFile,
    name: &str,
    hash: &str,
    credential_identity: Option<&str>,
    now: f64,
) -> Option<&'a McpCachedServerCatalog> {
    get_valid_cached_server(cache, name, hash, now).filter(|s| {
        let wanted = credential_identity.unwrap_or("none");
        s.credential_identity.as_deref() == Some(wanted)
            || (wanted == "none" && s.credential_identity.is_none())
    })
}
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
pub fn write_mcp_cached_server(
    agent_dir: &Path,
    name: &str,
    server: McpCachedServerCatalog,
) -> Result<(), CacheError> {
    // Without a valid installation key nothing keyed is persisted (senpi#3858).
    use crate::config_identity_key::is_ephemeral_identity;
    if server
        .config_hash
        .split('\0')
        .any(|part| is_ephemeral_identity(Some(part)))
        || is_ephemeral_identity(server.credential_identity.as_deref())
    {
        return Ok(());
    }
    // A catalog can echo a resolved reference value; it never reaches the disk.
    let persisted: McpCachedServerCatalog = serde_json::from_value(
        maho_core::config_reference_redaction::redact_resolved_reference_data(
            &serde_json::to_value(&server)?,
        ),
    )?;
    let mut cache = read_mcp_catalog_cache(agent_dir);
    cache.servers.insert(name.into(), persisted);
    let path = get_mcp_catalog_cache_path(agent_dir);
    let parent = agent_dir.join("cache");
    std::fs::create_dir_all(&parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    writeln!(tmp, "{}", serde_json::to_string_pretty(&cache)?)?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
pub async fn collect_server_catalog_for_cache(
    connection: &crate::connection::ServerConnection,
    timeout: std::time::Duration,
    config_hash: &str,
) -> Result<McpCachedServerCatalog, crate::errors::McpError> {
    collect_server_catalog_for_cache_in(connection, timeout, config_hash, None).await
}
/// Stamps the catalog with the credential it was listed under (catalog-identity.ts), keyed by `agent_dir`.
pub async fn collect_server_catalog_for_cache_in(
    connection: &crate::connection::ServerConnection,
    timeout: std::time::Duration,
    config_hash: &str,
    agent_dir: Option<&Path>,
) -> Result<McpCachedServerCatalog, crate::errors::McpError> {
    let credential_identity = agent_dir.and_then(|agent_dir| {
        crate::auth::catalog_identity::mcp_credential_identity(
            connection.config(),
            &connection.server_name,
            Some(agent_dir),
            connection.env(),
        )
    });
    use crate::catalog::collect_client_pages;
    let client = connection.client()?;
    let tools = if crate::modern::request::is_modern_mcp_client(&client) {
        // Modern servers list through the MRTR engine (tool-client.ts listMcpTools).
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |cursor| json!({"cursor":cursor}));
            let page = crate::tool_client::list_mcp_tools(
                &client,
                params,
                timeout,
                &crate::request_origin::McpRequestOptions::default(),
            )
            .await?;
            items.extend(
                page.get("tools")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
            cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        items
    } else {
        collect_client_pages(&client, "tools/list", "tools", timeout)
            .await?
            .items
    };
    let resources = collect_client_pages(&client, "resources/list", "resources", timeout)
        .await
        .map(|p| p.items)
        .unwrap_or_default();
    let prompts = collect_client_pages(&client, "prompts/list", "prompts", timeout)
        .await
        .map(|p| p.items)
        .unwrap_or_default();
    Ok(McpCachedServerCatalog {
        modern: crate::modern::request::is_modern_mcp_client(&client),
        credential_identity,
        config_hash: config_hash.into(),
        fetched_at: chrono::Utc::now()
            .timestamp_millis()
            .to_string()
            .parse()
            .unwrap_or(0.0),
        tools,
        resources,
        prompts,
        instructions: client.instructions.read().await.clone(),
    })
}
