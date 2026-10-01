use std::collections::BTreeMap;
use std::{sync::{Arc,Mutex},time::Duration};
use crate::{log::McpLogger,transport_sdk::McpClient};
use crate::{config_schema::{Auth,AuthMode,McpServerConfig,Transport},errors::{McpError,McpErrorKind},transport_sdk::McpTransportSpec};
pub fn create_mcp_transport_spec(server:&str,config:&McpServerConfig,env:Option<&BTreeMap<String,String>>)->Result<McpTransportSpec,McpError> {
    let failure=|kind,message| {let mut error=McpError::new(kind,message);error.phase=Some("create".into());error.server_name=Some(server.into());error};
    if config.transport==Some(Transport::Stdio) {
        let command=config.command.as_ref().filter(|s|!s.trim().is_empty()).ok_or_else(||failure(McpErrorKind::Connect,format!("MCP server {server} stdio command is required")))?;
        let mut merged=env.cloned().unwrap_or_default();merged.extend(config.env.clone().unwrap_or_default());
        return Ok(McpTransportSpec::Stdio {command:command.clone(),args:config.args.clone().unwrap_or_default(),cwd:config.cwd.clone(),env:merged});
    }
    let url=config.url.as_ref().filter(|s|!s.trim().is_empty()).ok_or_else(||failure(McpErrorKind::Connect,format!("MCP server {server} HTTP URL is required")))?;
    let url=url::Url::parse(url).map_err(|error|failure(McpErrorKind::Connect,format!("MCP server {server} HTTP URL is invalid: {error}")))?;
    let mut headers=config.headers.clone().unwrap_or_default();
    if config.auth==Some(Auth::Mode(AuthMode::Bearer)) || (config.auth.is_none() && config.bearer_token_env.is_some()) {
        let name=config.bearer_token_env.as_ref().filter(|s|!s.trim().is_empty()).ok_or_else(||failure(McpErrorKind::Auth,format!("MCP server {server} bearer auth requires bearerTokenEnv")))?;
        let token=env.and_then(|env|env.get(name)).cloned().or_else(||std::env::var(name).ok()).filter(|s|!s.is_empty()).ok_or_else(||failure(McpErrorKind::Auth,format!("MCP server {server} bearer token env {name} is not set")))?;
        headers.insert("authorization".into(),format!("Bearer {token}"));
    }
    Ok(McpTransportSpec::Http {url,headers})
}
pub struct McpTransportConnection {
    pub server_name:String,pub spec:McpTransportSpec,pub connect_timeout:Duration,
    logger:Arc<Mutex<McpLogger>>,client:tokio::sync::OnceCell<Arc<McpClient>>,
}
pub fn create_mcp_transport(server:&str,config:&McpServerConfig,env:Option<&BTreeMap<String,String>>,logger:Arc<Mutex<McpLogger>>)->Result<McpTransportConnection,McpError> {
    let spec=create_mcp_transport_spec(server,config,env)?;
    let timeout=config.connect_timeout_ms.unwrap_or(15000.0);
    Ok(McpTransportConnection {server_name:server.into(),spec,connect_timeout:Duration::from_secs_f64(timeout.max(0.0)/1000.0),logger,client:tokio::sync::OnceCell::new()})
}
impl McpTransportConnection {
    pub async fn materialize(&self)->Result<Arc<McpClient>,McpError> {
        self.client.get_or_try_init(||McpClient::materialize(&self.server_name,&self.spec,self.logger.clone())).await.cloned()
    }
    pub fn client(&self)->Result<Arc<McpClient>,McpError> {
        self.client.get().cloned().ok_or_else(||{
            let mut error=McpError::new(McpErrorKind::Connect,format!("MCP server {} transport is not started",self.server_name));error.phase=Some("create".into());error.server_name=Some(self.server_name.clone());error
        })
    }
    pub fn get_root_pid(&self)->Option<u32> {self.client.get().and_then(|client|client.root_pid)}
}
pub async fn connect_mcp_transport(connection:&McpTransportConnection)->Result<Arc<McpClient>,McpError> {
    let connect=async {let client=connection.materialize().await?;client.initialize(connection.connect_timeout).await?;Ok::<_,McpError>(client)};
    match tokio::time::timeout(connection.connect_timeout,connect).await {
        Ok(Ok(client))=>Ok(client),
        result=>{
            let _=shutdown_mcp_transport(connection).await;
            let mut error=match result {
                Ok(Err(error)) if error.kind!=McpErrorKind::Timeout=>McpError::new(McpErrorKind::Connect,format!("MCP server {} failed during connect: {error}",connection.server_name)),
                _=>McpError::new(McpErrorKind::Timeout,format!("MCP server {} timed out during connect after {}ms",connection.server_name,connection.connect_timeout.as_millis())),
            };
            error.phase=Some("connect".into());error.server_name=Some(connection.server_name.clone());error.retriable=true;Err(error)
        }
    }
}
pub async fn shutdown_mcp_transport(connection:&McpTransportConnection)->Result<(),McpError> {
    if let Some(client)=connection.client.get() {
        if let Some(pid)=client.root_pid {
            let reaper=crate::process_tree::reap_process_tree(pid,Duration::from_millis(400),Duration::from_millis(500));
            let (result,())=tokio::join!(client.close(),reaper);result?;
        }else{client.close().await?;}
    }
    Ok(())
}
