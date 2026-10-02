use crate::{config_schema::ResolvedMcpConfig,connection::ServerConnectionState,expose::{policy::CatalogIdentity,status::{McpServerExposureStatus,get_mcp_catalog_exposure_status,get_mcp_server_exposure_status}},service_types::McpConnectionEntry};
#[derive(Clone)]
struct CachedIdentity {server:String,tool:String}
impl CatalogIdentity for CachedIdentity {fn server(&self)->&str{&self.server}fn tool(&self)->&str{&self.tool}}
pub async fn get_mcp_service_exposure_status(name:&str,config:Option<&ResolvedMcpConfig>,entry:Option<&McpConnectionEntry>)->McpServerExposureStatus {
    let empty=||McpServerExposureStatus {hint:None,tool_count:None,mode:None};
    let (Some(config),Some(entry))=(config,entry) else{return empty();};
    let Some(server_config)=config.servers.get(name).and_then(|server|server.config.as_ref()) else{return empty();};
    if entry.connection.state()!=ServerConnectionState::Connected {
        if let Some(cached)=&entry.cached_catalog {
            let catalog=cached.tools.iter().map(|tool|CachedIdentity {server:name.into(),tool:tool.get("name").and_then(serde_json::Value::as_str).unwrap_or("").into()}).collect::<Vec<_>>();
            return get_mcp_catalog_exposure_status(&catalog,server_config,&config.settings);
        }
        return empty();
    }
    get_mcp_server_exposure_status(name,&entry.connection,server_config,&config.settings).await
}
