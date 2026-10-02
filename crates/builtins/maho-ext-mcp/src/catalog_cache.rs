use std::{collections::BTreeMap, io::Write, path::{Path,PathBuf}};
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct McpCachedServerCatalog { pub config_hash:String,pub fetched_at:f64,pub tools:Vec<Value>,pub resources:Vec<Value>,pub prompts:Vec<Value>,#[serde(skip_serializing_if="Option::is_none")] pub instructions:Option<String> }
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct McpCatalogCacheFile { pub version:u8,pub servers:BTreeMap<String,McpCachedServerCatalog> }
impl Default for McpCatalogCacheFile { fn default() -> Self { Self {version:1,servers:BTreeMap::new()} } }
pub fn get_mcp_catalog_cache_path(agent_dir:&Path) -> PathBuf { agent_dir.join("cache/mcp-cache.json") }
pub fn read_mcp_catalog_cache(agent_dir:&Path) -> McpCatalogCacheFile {
    match std::fs::read_to_string(get_mcp_catalog_cache_path(agent_dir)).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) { Some(value) => normalize_cache_file(&value),None => McpCatalogCacheFile::default() }
}
pub fn normalize_cache_file(value:&Value) -> McpCatalogCacheFile {
    if value.get("version") != Some(&json!(1)) { return McpCatalogCacheFile::default(); }
    let Some(servers) = value.get("servers").and_then(Value::as_object) else { return McpCatalogCacheFile::default(); };
    let mut cache = McpCatalogCacheFile::default();
    for (name,server) in servers { if let Some(catalog) = normalize_cached_server(server) { cache.servers.insert(name.clone(),catalog); } }
    cache
}
fn normalize_cached_server(value:&Value) -> Option<McpCachedServerCatalog> {
    let config_hash = value.get("configHash")?.as_str()?.to_owned();
    let fetched_at = value.get("fetchedAt")?.as_f64()?;
    let mut tools = Vec::new();
    for tool in value.get("tools")?.as_array()? {
        let name = tool.get("name")?.as_str()?;
        let schema = tool.get("inputSchema")?.as_object()?;
        let mut normalized = json!({"name":name,"inputSchema":schema});
        if let Some(description) = tool.get("description").filter(|v|v.is_string()) { normalized["description"]=description.clone(); }
        if let Some(annotations) = tool.get("annotations").filter(|v|v.is_object()) { normalized["annotations"]=annotations.clone(); }
        tools.push(normalized);
    }
    Some(McpCachedServerCatalog {config_hash,fetched_at,tools,resources:value.get("resources").and_then(Value::as_array).cloned().unwrap_or_default(),prompts:value.get("prompts").and_then(Value::as_array).cloned().unwrap_or_default(),instructions:value.get("instructions").and_then(Value::as_str).map(str::to_owned)})
}
pub fn get_valid_cached_server<'a>(cache:&'a McpCatalogCacheFile,name:&str,hash:&str,now:f64) -> Option<&'a McpCachedServerCatalog> {
    cache.servers.get(name).filter(|s|s.config_hash == hash && now-s.fetched_at <= 604800000.0)
}
#[derive(Debug,thiserror::Error)]
pub enum CacheError { #[error(transparent)] Io(#[from] std::io::Error),#[error(transparent)] Json(#[from] serde_json::Error) }
pub fn write_mcp_cached_server(agent_dir:&Path,name:&str,server:McpCachedServerCatalog) -> Result<(),CacheError> {
    let mut cache = read_mcp_catalog_cache(agent_dir); cache.servers.insert(name.into(),server);
    let path = get_mcp_catalog_cache_path(agent_dir);
    let parent = agent_dir.join("cache"); std::fs::create_dir_all(&parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    writeln!(tmp,"{}",serde_json::to_string_pretty(&cache)?)?;
    tmp.persist(path).map_err(|e|e.error)?; Ok(())
}
pub async fn collect_server_catalog_for_cache(connection:&crate::connection::ServerConnection,timeout:std::time::Duration,config_hash:&str)->Result<McpCachedServerCatalog,crate::errors::McpError> {
    use crate::catalog::collect_client_pages;
    let client=connection.client()?;
    let tools=collect_client_pages(&client,"tools/list","tools",timeout).await?.items;
    let resources=collect_client_pages(&client,"resources/list","resources",timeout).await.map(|p|p.items).unwrap_or_default();
    let prompts=collect_client_pages(&client,"prompts/list","prompts",timeout).await.map(|p|p.items).unwrap_or_default();
    Ok(McpCachedServerCatalog {config_hash:config_hash.into(),fetched_at:chrono::Utc::now().timestamp_millis().to_string().parse().unwrap_or(0.0),tools,resources,prompts,instructions:client.instructions.read().await.clone()})
}
