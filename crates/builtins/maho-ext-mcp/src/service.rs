use std::{collections::BTreeMap,path::{Path,PathBuf},sync::Arc};
use crate::{config_schema::{LoadMcpConfigOptions,ResolvedMcpConfig,McpServerState},config::{load_mcp_config,McpConfigValidationError},host_registry::HostMcpRegistry,service_connection::{McpSessionConnection,SessionConnectionOptions,create_mcp_session_connection,dispose_entry_connection},service_types::McpServerSnapshot};
#[derive(Debug,thiserror::Error)]
pub enum McpServiceError {
    #[error(transparent)] Config(#[from] McpConfigValidationError),
    #[error(transparent)] Logger(#[from] regex::Error),
    #[error(transparent)] Detach(#[from] crate::host_registry::RegistryDetachError),
    #[error(transparent)] Connection(#[from] crate::errors::McpError),
}
pub struct McpService {pub registry:Arc<HostMcpRegistry>,pub config:Option<ResolvedMcpConfig>,pub connections:BTreeMap<String,McpSessionConnection>,owner:u64,agent_dir:Option<PathBuf>}
impl McpService {
    pub fn new(registry:Arc<HostMcpRegistry>,owner:u64)->Self {Self {registry,config:None,connections:BTreeMap::new(),owner,agent_dir:None}}
    pub async fn attach_session(&mut self,cwd:&Path,agent_dir:&Path,env:&BTreeMap<String,String>,project_trusted:bool,declarations:&[maho_ext_api::RegisteredMcpServerDeclaration])->Result<(),McpServiceError> {
        let mut config=load_mcp_config(LoadMcpConfigOptions {cwd,agent_dir,env,project_trusted})?;
        crate::config::merge_extension_mcp_servers(&mut config,declarations)?;
        let existing=self.connections.keys().cloned().collect::<Vec<_>>();
        for name in existing {
            let entry=self.connections[&name].entry.lock().await;
            let retain=config.servers.get(&name).is_some_and(|server|server.state==McpServerState::Enabled && server.config_hash.as_deref()==Some(&entry.config_hash)) && self.agent_dir.as_deref()==Some(agent_dir);drop(entry);
            if !retain && let Some(connection)=self.connections.remove(&name){dispose_entry_connection(&connection,&self.registry,self.owner).await?;}
        }
        for (name,server) in &config.servers {
            if server.state!=McpServerState::Enabled || self.connections.contains_key(name){continue;}
            let (Some(server_config),Some(hash))=(&server.config,&server.config_hash) else{continue;};
            let key=format!("{name}:{hash}");
            let connection=create_mcp_session_connection(SessionConnectionOptions {registry:&self.registry,owner:self.owner,key:&key,name,config_hash:hash,config:server_config.clone(),agent_dir,env:Some(env.clone())})?;
            let cache=crate::catalog_cache::read_mcp_catalog_cache(agent_dir);
            if let Some(cached)=crate::catalog_cache::get_valid_cached_server(&cache,name,hash,chrono::Utc::now().timestamp_millis() as f64){connection.entry.lock().await.cached_catalog=Some(cached.clone());}
            self.connections.insert(name.clone(),connection);
        }
        self.agent_dir=Some(agent_dir.into());self.config=Some(config);Ok(())
    }
    pub async fn connect_server(&self,name:&str)->Result<(),McpServiceError> {
        if let (Some(connection),Some(config))=(self.connections.get(name),self.config.as_ref().and_then(|config|config.servers.get(name)).and_then(|server|server.config.as_ref())) {
            let mut entry=connection.entry.lock().await;
            crate::startup_race::connect_and_refresh_mcp_catalog(&mut entry,config).await;
            if let Some(error)=entry.connection.last_error(){return Err(error.into());}
        }
        Ok(())
    }
    pub async fn reconnect_server(&self,name:&str)->Result<(),McpServiceError> {if let Some(connection)=self.connections.get(name){connection.reconnect.reconnect_now().await?;}Ok(())}
    pub async fn server_snapshots(&self)->Vec<McpServerSnapshot> {
        let Some(config)=&self.config else{return Vec::new();};let mut snapshots=Vec::new();
        for (name,server) in &config.servers {
            let entry=if let Some(connection)=self.connections.get(name){Some(connection.entry.lock().await)}else{None};
            snapshots.push(crate::service_snapshot::build_mcp_server_snapshot(name,Some(server),entry.as_ref().map(|entry|entry.connection.as_ref()),entry.as_deref(),chrono::Utc::now().timestamp_millis() as f64));
        }
        snapshots
    }
    pub async fn dispose(&mut self)->Result<(),McpServiceError> {
        let connections=std::mem::take(&mut self.connections);
        for connection in connections.values(){dispose_entry_connection(connection,&self.registry,self.owner).await?;}
        self.config=None;self.agent_dir=None;Ok(())
    }
}
