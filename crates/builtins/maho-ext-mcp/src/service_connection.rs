use std::{collections::BTreeMap,path::Path,sync::{Arc,Mutex}};
use crate::{config_schema::McpServerConfig,connection::ServerConnection,errors::McpError,host_registry::HostMcpRegistry,log::McpLogger,service_types::{McpConnectionEntry,McpServerCounters},idle::McpConnectionLifecycle,reconnect::McpReconnect};
pub struct McpSessionConnection {pub entry:Arc<tokio::sync::Mutex<McpConnectionEntry>>,pub lifecycle:Arc<McpConnectionLifecycle>,pub reconnect:Arc<McpReconnect>}
pub struct SessionConnectionOptions<'a> {pub registry:&'a HostMcpRegistry,pub owner:u64,pub key:&'a str,pub name:&'a str,pub config_hash:&'a str,pub config:McpServerConfig,pub agent_dir:&'a Path,pub env:Option<BTreeMap<String,String>>}
pub fn create_mcp_session_connection(options:SessionConnectionOptions<'_>)->Result<McpSessionConnection,regex::Error> {
    let SessionConnectionOptions {registry,owner,key,name,config_hash,config,agent_dir,env}=options;
    let logger=Arc::new(Mutex::new(McpLogger::new(name,agent_dir,None)?));
    let plan=crate::auth::context::resolve_server_auth(name,&config,agent_dir,None,reqwest::Client::new());
    if let Ok(warnings)=crate::auth::context::detect_literal_bearer_warnings(name,&config) {for warning in warnings {let _=logger.lock().unwrap_or_else(std::sync::PoisonError::into_inner).log("warning",&warning,None,None);}}
    let connection=registry.attach(key,owner,||ServerConnection::new(name,config.clone(),env,logger.clone()),false);
    if let Some(refresh)=plan.refresh {connection.set_auth(Arc::new(refresh));}
    let entry=Arc::new(tokio::sync::Mutex::new(McpConnectionEntry {key:key.into(),name:name.into(),config_hash:config_hash.into(),connection:connection.clone(),logger,created_at_ms:chrono::Utc::now().timestamp_millis() as f64,counters:McpServerCounters::default(),agent_dir:Some(agent_dir.into()),cached_catalog:None,cache_refreshed_after_connect:false}));
    let lifecycle=McpConnectionLifecycle::configure(connection.clone(),config.clone());
    let weak=Arc::downgrade(&entry);
    let reconnect=McpReconnect::configure(connection,Arc::new(move ||{let weak=weak.clone();let config=config.clone();Box::pin(async move {
        let Some(entry)=weak.upgrade() else{return Ok(());};let mut entry=entry.lock().await;entry.counters.reconnect_count+=1;entry.cache_refreshed_after_connect=false;
        entry.connection.renew().await?;crate::startup_race::connect_and_refresh_mcp_catalog(&mut entry,&config).await;Ok::<_,McpError>(())
    })}),Arc::new(||true),Arc::new(||{let mut bytes=[0u8;8];getrandom::fill(&mut bytes).expect("OS entropy unavailable for MCP reconnect jitter");(u64::from_le_bytes(bytes)>>11) as f64 / ((1u64<<53) as f64)}));
    Ok(McpSessionConnection {entry,lifecycle,reconnect})
}
pub async fn dispose_entry_connection(entry:&McpSessionConnection,registry:&HostMcpRegistry,owner:u64)->Result<(),crate::host_registry::RegistryDetachError> {
    entry.reconnect.dispose();entry.lifecycle.dispose();let key=entry.entry.lock().await.key.clone();registry.detach(&key,owner).await
}
