use std::sync::Arc;
use crate::service::McpService;
pub async fn handle_mcp_auth_command(subcommand:&str,args:&[String],has_ui:bool,ui:Arc<dyn maho_ext_api::ExtensionUi>,service:&mut McpService)->Result<String,String> {
    let name=args.first().map_or("",String::as_str);
    if name.is_empty() || !service.config.as_ref().is_some_and(|config|config.servers.contains_key(name)) {return Err(format!("Unknown MCP server: {}",if name.is_empty(){"<missing>"}else{name}));}
    match subcommand {
        "auth-start"=>service.auth_start(name).await.map(|url|format!("Open this URL, approve, then run /mcp auth-complete {name} <redirect-url>:\n{url}")).map_err(|error|error.to_string()),
        "auth"=>service.auth(name,has_ui,|url|async move {ui.notify(&format!("Open this URL to authorize {name}:\n{url}"),maho_ext_api::NotificationType::Info);Ok(())}).await.map(|url|url.map_or_else(||format!("MCP server {name} authorized"),|url|format!("Open this URL, then /mcp auth-complete {name} <redirect-url>:\n{url}"))).map_err(|error|error.to_string()),
        "auth-complete"=>{
            let redirect=args.get(1).map_or("",String::as_str);
            if redirect.is_empty(){return Err(format!("Usage: /mcp auth-complete {name} <redirect-url>"));}
            service.auth_complete(name,redirect).await.map(|()|format!("MCP server {name} authorized")).map_err(|error|error.to_string())
        }
        "logout"=>service.logout(name).await.map(|()|format!("MCP server {name} logged out")).map_err(|error|error.to_string()),
        _=>Err(format!("Unknown MCP auth subcommand: {subcommand}")),
    }
}
