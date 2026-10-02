use std::time::Duration;
pub async fn connect_and_refresh_mcp_catalog(entry:&mut crate::service_types::McpConnectionEntry,config:&crate::config_schema::McpServerConfig)->Result<(),crate::errors::McpError> {
    if let Err(error)=entry.auth_plan.ensure_fresh().await {
        return Err(crate::health::mark_mcp_connection_needs_auth(&entry.connection,&error).unwrap_or(error));
    }
    let client=match entry.connection.connect().await {
        Ok(client)=>client,
        Err(error)=>{let _=entry.logger.lock().unwrap_or_else(std::sync::PoisonError::into_inner).log("warning",&error.to_string(),None,None);return Ok(());}
    };
    if entry.cache_refreshed_after_connect{return Ok(());}
    entry.cache_refreshed_after_connect=true;
    let result=crate::catalog_cache::collect_server_catalog_for_cache(&entry.connection,Duration::from_secs_f64(config.request_timeout_ms.unwrap_or(30000.0)/1000.0),&entry.config_hash).await;
    match result {
        Ok(catalog)=>{
            if let Some(agent_dir)=&entry.agent_dir && let Err(error)=crate::catalog_cache::write_mcp_cached_server(agent_dir,&entry.name,catalog.clone()) {let _=entry.logger.lock().unwrap_or_else(std::sync::PoisonError::into_inner).log("warning",&format!("Failed to refresh MCP catalog cache: {error}"),None,None);}
            crate::resources::ensure_mcp_resource_subscriptions(client.clone(),&catalog.resources,Duration::from_secs_f64(config.request_timeout_ms.unwrap_or(30000.0)/1000.0)).await;
            entry.cached_catalog=Some(catalog);
        }
        Err(error)=>{let _=entry.logger.lock().unwrap_or_else(std::sync::PoisonError::into_inner).log("warning",&format!("Failed to refresh MCP catalog cache: {error}"),None,None);}
    }
    Ok(())
}
pub const MCP_STARTUP_RACE_MS:f64=250.0;
pub const MCP_STARTUP_TIMEOUT_ENV:&str="SENPI_MCP_STARTUP_TIMEOUT_MS";
pub const MCP_ATTACH_SETTLE_TIMEOUT_MS:u64=5000;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum McpStartupRaceResult {Settled,Timeout}
#[derive(Default)]
pub struct McpDeferredAttach {
    pending:std::sync::Mutex<Vec<tokio::sync::watch::Receiver<bool>>>,
}
impl McpDeferredAttach {
    pub fn track(&self,settled:impl std::future::Future<Output=()>+Send+'static) {
        let (sender,receiver)=tokio::sync::watch::channel(false);
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(receiver);
        tokio::spawn(async move {settled.await;sender.send_replace(true);});
    }
    pub async fn wait(&self,timeout:Duration)->McpStartupRaceResult {
        let pending={
            let mut pending=self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            pending.retain(|receiver|!*receiver.borrow());
            pending.clone()
        };
        if pending.is_empty(){return McpStartupRaceResult::Settled;}
        let settle=async move {
            for mut receiver in pending {
                while !*receiver.borrow() {
                    if receiver.changed().await.is_err(){break;}
                }
            }
        };
        match tokio::time::timeout(timeout,settle).await {
            Ok(())=>McpStartupRaceResult::Settled,
            Err(_)=>McpStartupRaceResult::Timeout,
        }
    }
    pub fn clear(&self) {
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    }
}
pub fn resolve_mcp_startup_timeout_ms(configured:Option<f64>,environment:Option<&str>)->f64 {
    if let Some(raw)=environment.map(str::trim).filter(|raw|!raw.is_empty()) {
        let parsed=if let Some(hex)=raw.strip_prefix("0x").or_else(||raw.strip_prefix("0X")) {u64::from_str_radix(hex,16).ok().map(|value|value as f64)} else if let Some(binary)=raw.strip_prefix("0b").or_else(||raw.strip_prefix("0B")) {u64::from_str_radix(binary,2).ok().map(|value|value as f64)} else if let Some(octal)=raw.strip_prefix("0o").or_else(||raw.strip_prefix("0O")) {u64::from_str_radix(octal,8).ok().map(|value|value as f64)} else {raw.parse::<f64>().ok()};
        if let Some(value)=parsed.filter(|value|value.is_finite() && *value>=0.0){return value;}
    }
    configured.unwrap_or(MCP_STARTUP_RACE_MS)
}
pub async fn wait_for_mcp_startup_race<T,E>(connect:impl std::future::Future<Output=Result<T,E>>,deadline:Duration)->Result<McpStartupRaceResult,E> {
    tokio::select! {
        result=connect=>result.map(|_|McpStartupRaceResult::Settled),
        ()=tokio::time::sleep(deadline)=>Ok(McpStartupRaceResult::Timeout),
    }
}
pub fn should_race_mcp_startup(lifecycle:crate::config_schema::Lifecycle)->bool {
    matches!(lifecycle,crate::config_schema::Lifecycle::Eager|crate::config_schema::Lifecycle::KeepAlive)
}
