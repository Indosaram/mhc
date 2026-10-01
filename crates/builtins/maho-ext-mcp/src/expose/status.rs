use std::{sync::Arc,time::Duration};
use crate::{catalog::{McpToolCatalogEntry,cached_tools_to_catalog_entries},config_schema::{Exposure,McpServerConfig,McpSettings},transport_sdk::McpClient};
use super::policy::{CatalogIdentity,compute_mcp_exposure_policy};
#[derive(Debug,PartialEq)]
pub struct McpServerExposureStatus {pub hint:Option<String>,pub tool_count:Option<usize>,pub mode:Option<Exposure>}
pub fn get_mcp_catalog_exposure_status<T:CatalogIdentity+Clone>(catalog:&[T],config:&McpServerConfig,settings:&McpSettings)->McpServerExposureStatus {
    let policy=compute_mcp_exposure_policy(catalog,config,settings);
    let hint=if policy.filtered_entries.is_empty(){Some("No MCP tools matched includeTools/excludeTools filters.".into())}
        else if policy.mode==Exposure::Search{Some(format!("search mode: {} active now, {} searchable via tool_search",policy.active_entries.len(),policy.registered_entries.len()))}else{None};
    McpServerExposureStatus {hint,tool_count:Some(policy.registered_entries.len()),mode:Some(policy.mode)}
}
pub async fn get_mcp_server_exposure_status(name:&str,client:Arc<McpClient>,config:&McpServerConfig,settings:&McpSettings)->McpServerExposureStatus {
    match client.request("tools/list",serde_json::json!({}),Duration::from_millis(500)).await {
        Ok(result)=>{
            let tools:&[serde_json::Value]=result.get("tools").and_then(serde_json::Value::as_array).map_or(&[],Vec::as_slice);
            let catalog:Vec<McpToolCatalogEntry>=cached_tools_to_catalog_entries(name,tools,client,Duration::from_secs(30));get_mcp_catalog_exposure_status(&catalog,config,settings)
        }
        Err(_)=>McpServerExposureStatus {hint:None,tool_count:None,mode:None},
    }
}
