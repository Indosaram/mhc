use std::{sync::Arc,time::Duration};
use serde_json::{Value,json};
use crate::{errors::McpError,transport_sdk::McpClient,expose::{pagination::{collect_all_pages,McpListPage,McpPaginationResult},policy::CatalogIdentity}};
#[derive(Clone)]
pub struct McpToolCatalogEntry {
    pub server:String,pub tool:String,pub schema:Value,pub description:Option<String>,pub annotations:Option<Value>,
    pub request_timeout:Duration,pub client:Arc<McpClient>,
    pub runtime:Option<Arc<McpCatalogRuntime>>,
}
pub struct McpCatalogRuntime {
    pub connection:Arc<crate::connection::ServerConnection>,
    pub lifecycle:Arc<crate::idle::McpConnectionLifecycle>,
    pub health:crate::health::McpHealthState,
}
impl CatalogIdentity for McpToolCatalogEntry {fn server(&self)->&str{&self.server}fn tool(&self)->&str{&self.tool}}
pub async fn collect_client_pages(client:&McpClient,method:&str,key:&str,timeout:Duration)->Result<McpPaginationResult<Value>,McpError> {
    collect_all_pages(|cursor|async move {
        let params=cursor.map_or_else(||json!({}),|cursor|json!({"cursor":cursor}));
        let response=client.request(method,params,timeout).await?;
        Ok(McpListPage {items:response.get(key).and_then(Value::as_array).cloned(),next_cursor:response.get("nextCursor").and_then(Value::as_str).map(str::to_owned),..Default::default()})
    }).await
}
pub async fn collect_tool_catalog(server:&str,client:Arc<McpClient>,timeout:Duration)->Result<Vec<McpToolCatalogEntry>,McpError> {
    let listed=collect_client_pages(&client,"tools/list","tools",timeout).await?;
    Ok(cached_tools_to_catalog_entries(server,&listed.items,client,timeout))
}
pub fn cached_tools_to_catalog_entries(server:&str,tools:&[Value],client:Arc<McpClient>,timeout:Duration)->Vec<McpToolCatalogEntry> {
    tools.iter().map(|tool|McpToolCatalogEntry {server:server.into(),tool:tool.get("name").and_then(Value::as_str).unwrap_or("").into(),schema:tool.get("inputSchema").cloned().unwrap_or(Value::Null),description:tool.get("description").and_then(Value::as_str).map(str::to_owned),annotations:tool.get("annotations").cloned(),request_timeout:timeout,client:client.clone(),runtime:None}).collect()
}
