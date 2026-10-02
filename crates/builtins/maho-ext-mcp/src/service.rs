use std::{collections::BTreeMap,path::{Path,PathBuf},sync::Arc};
use crate::{config_schema::{LoadMcpConfigOptions,ResolvedMcpConfig,McpServerState},config::{load_mcp_config,McpConfigValidationError},host_registry::HostMcpRegistry,service_connection::{McpSessionConnection,SessionConnectionOptions,create_mcp_session_connection,dispose_entry_connection},service_types::McpServerSnapshot};
#[derive(Debug,thiserror::Error)]
pub enum McpServiceError {
    #[error(transparent)] Config(#[from] McpConfigValidationError),
    #[error(transparent)] Logger(#[from] regex::Error),
    #[error(transparent)] Detach(#[from] crate::host_registry::RegistryDetachError),
    #[error(transparent)] Connection(#[from] crate::errors::McpError),
    #[error(transparent)] OAuth(#[from] crate::auth::oauth::OAuthRequestError),
}
pub struct McpService {pub registry:Arc<HostMcpRegistry>,pub config:Option<ResolvedMcpConfig>,pub connections:BTreeMap<String,McpSessionConnection>,owner:u64,agent_dir:Option<PathBuf>,deferred:crate::startup_race::McpDeferredAttach,pending_auth:BTreeMap<String,crate::auth::oauth_provider::McpOAuthProvider>,elicitation_ui:Option<Arc<dyn maho_ext_api::ExtensionUi>>}
impl McpService {
    pub fn new(registry:Arc<HostMcpRegistry>,owner:u64)->Self {Self {registry,config:None,connections:BTreeMap::new(),owner,agent_dir:None,deferred:Default::default(),pending_auth:BTreeMap::new(),elicitation_ui:None}}
    pub fn set_elicitation_ui(&mut self,ui:Option<Arc<dyn maho_ext_api::ExtensionUi>>){self.elicitation_ui=ui;}
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
            connection.entry.lock().await.connection.set_elicitation_ui(self.elicitation_ui.clone());
            let cache=crate::catalog_cache::read_mcp_catalog_cache(agent_dir);
            if let Some(cached)=crate::catalog_cache::get_valid_cached_server(&cache,name,hash,chrono::Utc::now().timestamp_millis() as f64){connection.entry.lock().await.cached_catalog=Some(cached.clone());}
            if crate::startup_race::should_race_mcp_startup(server_config.lifecycle.unwrap_or(crate::config_schema::Lifecycle::Lazy)) {
                let entry=connection.entry.clone();let server_config=server_config.clone();
                let timeout=crate::startup_race::resolve_mcp_startup_timeout_ms(server_config.startup_timeout_ms,env.get(crate::startup_race::MCP_STARTUP_TIMEOUT_ENV).map(String::as_str));
                let (sender,mut settled)=tokio::sync::watch::channel(false);
                self.deferred.track(async move {let mut entry=entry.lock().await;crate::startup_race::connect_and_refresh_mcp_catalog(&mut entry,&server_config).await;sender.send_replace(true);});
                let _=tokio::time::timeout(std::time::Duration::from_secs_f64(timeout/1000.0),async {while !*settled.borrow(){if settled.changed().await.is_err(){break;}}}).await;
            }
            self.connections.insert(name.clone(),connection);
        }
        self.agent_dir=Some(agent_dir.into());self.config=Some(config);Ok(())
    }
    pub async fn connect_server(&self,name:&str)->Result<(),McpServiceError> {
        if !self.connections.contains_key(name){return Err(crate::errors::McpError::new(crate::errors::McpErrorKind::Connect,format!("Unknown MCP server: {name}")).into());}
        if let (Some(connection),Some(config))=(self.connections.get(name),self.config.as_ref().and_then(|config|config.servers.get(name)).and_then(|server|server.config.as_ref())) {
            let mut entry=connection.entry.lock().await;
            crate::startup_race::connect_and_refresh_mcp_catalog(&mut entry,config).await;
            if let Some(error)=entry.connection.last_error(){return Err(error.into());}
        }
        Ok(())
    }
    pub async fn reconnect_server(&self,name:&str)->Result<(),McpServiceError> {let connection=self.connections.get(name).ok_or_else(||crate::errors::McpError::new(crate::errors::McpErrorKind::Connect,format!("Unknown MCP server: {name}")))?;connection.reconnect.reconnect_now().await?;Ok(())}
    fn auth_provider(&self,name:&str)->Result<crate::auth::oauth_provider::McpOAuthProvider,McpServiceError> {
        let config=self.config.as_ref().and_then(|config|config.servers.get(name)).and_then(|server|server.config.as_ref()).ok_or_else(||crate::errors::McpError::new(crate::errors::McpErrorKind::Auth,format!("Unknown MCP server: {name}")))?;
        let agent_dir=self.agent_dir.as_ref().ok_or_else(||crate::errors::McpError::new(crate::errors::McpErrorKind::Auth,"MCP session is not attached"))?;
        Ok(crate::auth::commands_auth::build_provider(name,config,agent_dir,self.config.as_ref().and_then(|config|config.settings.oauth_callback_url.as_deref()))?)
    }
    pub async fn auth_start(&mut self,name:&str)->Result<String,McpServiceError> {let provider=self.auth_provider(name)?;Ok(crate::auth::commands_auth::run_auth_start(name,provider,&mut self.pending_auth,&reqwest::Client::new()).await?)}
    pub async fn auth<F,Fut>(&mut self,name:&str,has_ui:bool,on_authorization:F)->Result<Option<String>,McpServiceError>
    where F:FnOnce(url::Url)->Fut,Fut:std::future::Future<Output=Result<(),crate::auth::oauth::OAuthRequestError>> {
        let provider=self.auth_provider(name)?;
        let config=self.config.as_ref().and_then(|config|config.servers.get(name)).and_then(|server|server.config.as_ref());
        let oauth=config.and_then(|config|config.oauth.as_ref());
        if oauth.is_some_and(|oauth|oauth.flow==Some(crate::config_schema::OAuthFlow::ClientCredentials)) {crate::auth::commands_auth::run_client_credentials_auth(&provider,&reqwest::Client::new()).await?;self.reconnect_server(name).await?;return Ok(None);}
        if !has_ui{return Err(crate::errors::McpError::new(crate::errors::McpErrorKind::Auth,format!("MCP server {name} needs interactive OAuth. Run senpi in a terminal, then: /mcp auth-start {name} and /mcp auth-complete {name} <redirect-url>")).into());}
        if self.config.as_ref().and_then(|config|config.settings.oauth_callback_url.as_ref()).is_some(){return self.auth_start(name).await.map(Some);}
        let port=oauth.filter(|oauth|oauth.client_id.is_some() && oauth.client_metadata_url.is_none()).and_then(|oauth|oauth.callback_port);
        crate::auth::commands_auth::run_loopback_auth(provider,port,&reqwest::Client::new(),on_authorization).await?;self.reconnect_server(name).await?;Ok(None)
    }
    pub async fn auth_complete(&mut self,name:&str,redirect:&str)->Result<(),McpServiceError> {crate::auth::commands_auth::run_auth_complete(name,redirect,&mut self.pending_auth,&reqwest::Client::new()).await?;self.reconnect_server(name).await}
    pub async fn logout(&mut self,name:&str)->Result<(),McpServiceError> {let provider=self.auth_provider(name)?;crate::auth::commands_auth::run_logout(name,&provider,&mut self.pending_auth)?;if let Some(connection)=self.connections.get(name){let entry=connection.entry.lock().await;entry.connection.mark_failure(crate::connection::ServerConnectionState::NeedsAuth,Some(crate::errors::McpError::new(crate::errors::McpErrorKind::Auth,"MCP credentials cleared")));}Ok(())}
    pub async fn wait_for_deferred_attach(&self,timeout:std::time::Duration)->crate::startup_race::McpStartupRaceResult {self.deferred.wait(timeout).await}
    pub async fn server_snapshots(&self)->Vec<McpServerSnapshot> {
        let Some(config)=&self.config else{return Vec::new();};let mut snapshots=Vec::new();
        for (name,server) in &config.servers {
            let entry=if let Some(connection)=self.connections.get(name){Some(connection.entry.lock().await)}else{None};
            snapshots.push(crate::service_snapshot::build_mcp_server_snapshot(name,Some(server),entry.as_ref().map(|entry|entry.connection.as_ref()),entry.as_deref(),chrono::Utc::now().timestamp_millis() as f64));
        }
        snapshots
    }
    pub async fn status(&self,title:&str)->String {
        let mut rows=Vec::new();
        for snapshot in self.server_snapshots().await {
            let entry=if let Some(connection)=self.connections.get(&snapshot.name){Some(connection.entry.lock().await)}else{None};
            let exposure=crate::service_exposure::get_mcp_service_exposure_status(&snapshot.name,self.config.as_ref(),entry.as_deref()).await;
            rows.push(crate::status::McpStatusRow {snapshot,exposure});
        }
        crate::status::format_mcp_status(title,&rows)
    }
    pub async fn test_server(&self,name:&str)->Result<(f64,usize),McpServiceError> {
        let started=tokio::time::Instant::now();self.connect_server(name).await?;
        let connection=self.connections.get(name).ok_or_else(||crate::errors::McpError::new(crate::errors::McpErrorKind::Connect,format!("Unknown MCP server: {name}")))?;
        let mut entry=connection.entry.lock().await;let result=entry.connection.client()?.request("tools/list",serde_json::json!({}),std::time::Duration::from_secs(2)).await;
        let elapsed=started.elapsed().as_secs_f64()*1000.0;entry.counters.call_count+=1;entry.counters.total_latency_ms+=elapsed;
        if result.is_err(){entry.counters.error_count+=1;}
        Ok((elapsed,result?.get("tools").and_then(serde_json::Value::as_array).map_or(0,Vec::len)))
    }
    pub async fn dispose(&mut self)->Result<(),McpServiceError> {
        let connections=std::mem::take(&mut self.connections);
        for connection in connections.values(){dispose_entry_connection(connection,&self.registry,self.owner).await?;}
        self.config=None;self.agent_dir=None;self.deferred.clear();self.pending_auth.clear();Ok(())
    }
}
