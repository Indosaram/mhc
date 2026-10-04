use std::{collections::BTreeMap,path::Path,sync::{Arc,Mutex}};
use crate::{config_schema::McpServerConfig,connection::ServerConnection,errors::McpError,host_registry::HostMcpRegistry,log::McpLogger,service_types::{McpConnectionEntry,McpServerCounters},idle::McpConnectionLifecycle,reconnect::McpReconnect};
pub struct McpSessionConnection {pub entry:Arc<tokio::sync::Mutex<McpConnectionEntry>>,pub lifecycle:Option<Arc<McpConnectionLifecycle>>,pub reconnect:Option<Arc<McpReconnect>>,pub shared:Option<Arc<crate::shared_lease::SharedMcpLease>>}
pub struct SessionConnectionOptions<'a> {pub registry:&'a HostMcpRegistry,pub owner:u64,pub key:&'a str,pub name:&'a str,pub config_hash:&'a str,pub config:McpServerConfig,pub agent_dir:&'a Path,pub env:Option<BTreeMap<String,String>>}
#[derive(Debug,thiserror::Error)]
pub enum SessionConnectionError {#[error(transparent)] Logger(#[from] regex::Error),#[error(transparent)] Connection(#[from] McpError)}
pub fn create_mcp_session_connection(options:SessionConnectionOptions<'_>)->Result<McpSessionConnection,SessionConnectionError> {
    create_session_connection(options,None)
}
pub fn create_shared_mcp_session_connection(options:SessionConnectionOptions<'_>,cwd:&str)->Result<McpSessionConnection,SessionConnectionError> {
    create_session_connection(options,Some(cwd))
}
fn create_session_connection(options:SessionConnectionOptions<'_>,shared_cwd:Option<&str>)->Result<McpSessionConnection,SessionConnectionError> {
    let SessionConnectionOptions {registry,owner,key,name,config_hash,config,agent_dir,env}=options;
    let logger=Arc::new(Mutex::new(McpLogger::new(name,agent_dir,None)?));
    let plan=crate::auth::context::resolve_server_auth_with(crate::auth::context::ServerAuthDeps {server_name:name,config:&config,agent_dir:Some(agent_dir),logger:Some(logger.clone()),redirect_url:None,on_redirect:None,client:reqwest::Client::new()});
    if let Ok(warnings)=crate::auth::context::detect_literal_bearer_warnings(name,&config) {for warning in warnings {let _=logger.lock().unwrap_or_else(std::sync::PoisonError::into_inner).log("warning",&warning,None,None);}}
    let refresh=plan.refresh.clone();
    let shared=shared_cwd.filter(|cwd|crate::sharing_policy::shareable(&config,Some(cwd))).map(|_|registry.attach_shared(key,owner,||{let connection=ServerConnection::new(name,config.clone(),env.clone(),logger.clone());if let Some(refresh)=refresh.clone(){connection.set_auth(refresh);}crate::shared_connection::SharedMcpConnection::with_idle_timeout(connection,agent_dir.into(),config_hash.into(),std::time::Duration::from_secs_f64(config.request_timeout_ms.unwrap_or(30000.0)/1000.0),std::time::Duration::from_secs_f64(config.idle_timeout_min.unwrap_or(10.0)*60.0))})).transpose()?;
    let connection=match &shared {Some(lease)=>lease.base_connection(),None=>registry.attach(key,owner,||ServerConnection::new(name,config.clone(),env,logger.clone()),false)};
    if shared.is_none() && let Some(refresh)=refresh {connection.set_auth(refresh);}
    let entry=Arc::new(tokio::sync::Mutex::new(McpConnectionEntry {key:key.into(),name:name.into(),config_hash:config_hash.into(),connection:connection.clone(),logger,created_at_ms:chrono::Utc::now().timestamp_millis() as f64,counters:McpServerCounters::default(),agent_dir:Some(agent_dir.into()),cached_catalog:None,cache_refreshed_after_connect:false,auth_plan:plan,artifacts:None}));
    if shared.is_some(){return Ok(McpSessionConnection {entry,lifecycle:None,reconnect:None,shared});}
    let lifecycle=McpConnectionLifecycle::configure(connection.clone(),config.clone());
    let weak=Arc::downgrade(&entry);
    let reconnect=McpReconnect::configure(connection,Arc::new(move ||{let weak=weak.clone();let config=config.clone();Box::pin(async move {
        let Some(entry)=weak.upgrade() else{return Ok(());};let mut entry=entry.lock().await;entry.counters.reconnect_count+=1;entry.cache_refreshed_after_connect=false;
        if let Err(error)=entry.auth_plan.ensure_fresh().await {return Err(crate::health::mark_mcp_connection_needs_auth(&entry.connection,&error).unwrap_or(error));}
        entry.connection.renew().await?;crate::startup_race::connect_and_refresh_mcp_catalog(&mut entry,&config).await?;if let Some(error)=entry.connection.last_error(){return Err(error);}Ok::<_,McpError>(())
    })}),Arc::new(||true),Arc::new(||{let mut bytes=[0u8;8];getrandom::fill(&mut bytes).expect("OS entropy unavailable for MCP reconnect jitter");(u64::from_le_bytes(bytes)>>11) as f64 / ((1u64<<53) as f64)}));
    Ok(McpSessionConnection {entry,lifecycle:Some(lifecycle),reconnect:Some(reconnect),shared:None})
}
pub async fn dispose_entry_connection(entry:&McpSessionConnection,registry:&HostMcpRegistry,owner:u64)->Result<(),crate::host_registry::RegistryDetachError> {
    if let Some(lease)=&entry.shared {lease.dispose();return Ok(());}
    if let Some(reconnect)=&entry.reconnect {reconnect.dispose();}
    if let Some(lifecycle)=&entry.lifecycle {lifecycle.dispose();}
    let key=entry.entry.lock().await.key.clone();registry.detach(&key,owner).await
}
