use std::{collections::BTreeMap,path::PathBuf,sync::{Arc,Mutex},time::Duration};
use crate::{connection::ServerConnection,errors::{McpError,McpErrorKind},catalog_cache::{McpCachedServerCatalog,collect_server_catalog_for_cache,write_mcp_cached_server},transport_sdk::McpClient};
struct SharedState {leases:BTreeMap<u64,usize>,calls:BTreeMap<u64,usize>,disposed:bool}
pub struct SharedMcpConnection {
    pub connection:Arc<ServerConnection>,state:Mutex<SharedState>,catalog:tokio::sync::Mutex<Option<(u64,McpCachedServerCatalog)>>,
    renewal:tokio::sync::Mutex<()>,agent_dir:PathBuf,config_hash:String,request_timeout:Duration,
    subscription:Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl SharedMcpConnection {
    pub fn new(connection:Arc<ServerConnection>,agent_dir:PathBuf,config_hash:String,request_timeout:Duration)->Arc<Self> {
        let mut changes=connection.on_tools_changed();
        let shared=Arc::new(Self {connection,state:Mutex::new(SharedState {leases:BTreeMap::new(),calls:BTreeMap::new(),disposed:false}),catalog:tokio::sync::Mutex::new(None),renewal:tokio::sync::Mutex::new(()),agent_dir,config_hash,request_timeout,subscription:Mutex::new(None)});
        let weak=Arc::downgrade(&shared);let task=tokio::spawn(async move {loop {match changes.recv().await {Ok(_)|Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>{let Some(shared)=weak.upgrade() else{return;};*shared.catalog.lock().await=None;},Err(_)=>return}}});
        *shared.subscription.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=Some(task);shared
    }
    pub fn attach(self:&Arc<Self>,owner:u64,key:String)->Result<Arc<crate::shared_lease::SharedMcpLease>,McpError> {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.disposed{return Err(McpError::new(McpErrorKind::Connect,"MCP shared connection disposed"));}
        *state.leases.entry(owner).or_default()+=1;
        Ok(crate::shared_lease::SharedMcpLease::new(self.clone(),owner,key))
    }
    pub fn release(&self,owner:u64) {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(count)=state.leases.get_mut(&owner) {*count-=1;if *count==0 {state.leases.remove(&owner);}}
    }
    pub fn lease_count(&self)->usize {self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).leases.values().sum()}
    pub fn owner_attached(&self,owner:u64)->bool {let state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);!state.disposed && state.leases.contains_key(&owner)}
    pub fn elicitation_owner(&self)->Option<u64> {
        let state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.calls.len()!=1{return None;}
        state.calls.keys().next().copied().filter(|owner|state.leases.contains_key(owner))
    }
    pub async fn connect(&self)->Result<Arc<McpClient>,McpError> {
        let _renewal=self.renewal.lock().await;
        if self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).disposed{return Err(McpError::new(McpErrorKind::Connect,"MCP shared connection disposed"));}
        if matches!(self.connection.state(),crate::connection::ServerConnectionState::Connected|crate::connection::ServerConnectionState::Connecting|crate::connection::ServerConnectionState::Idle){self.connection.connect().await}else{self.connection.renew().await}
    }
    pub async fn call_tool<T>(self:&Arc<Self>,owner:u64,call:impl std::future::Future<Output=T>)->T {
        {let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);*state.calls.entry(owner).or_default()+=1;}
        struct CallGuard {shared:Arc<SharedMcpConnection>,owner:u64}
        impl Drop for CallGuard {fn drop(&mut self){let mut state=self.shared.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if let Some(count)=state.calls.get_mut(&self.owner){*count-=1;if *count==0{state.calls.remove(&self.owner);}}}}
        let _guard=CallGuard {shared:self.clone(),owner};call.await
    }
    pub async fn catalog(&self,refresh:bool)->Result<McpCachedServerCatalog,McpError> {
        let client=self.connect().await?;let mut cached=self.catalog.lock().await;let generation=self.connection.generation();
        if !refresh && let Some((previous,catalog))=&*cached && *previous==generation{return Ok(catalog.clone());}
        let catalog=collect_server_catalog_for_cache(&client,self.request_timeout,&self.config_hash).await?;
        if self.connection.generation()==generation {
            if cached.as_ref().is_none_or(|(previous,_)|*previous!=generation){write_mcp_cached_server(&self.agent_dir,&self.connection.server_name,catalog.clone()).map_err(|error|McpError::new(McpErrorKind::Connect,error.to_string()))?;}
            *cached=Some((generation,catalog.clone()));
        }
        Ok(catalog)
    }
    pub async fn dispose(&self)->Result<(),McpError> {
        {let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if state.disposed{return Ok(());}state.disposed=true;state.leases.clear();}
        if let Some(task)=self.subscription.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take(){task.abort();}
        self.connection.dispose().await
    }
}
impl Drop for SharedMcpConnection {fn drop(&mut self){if let Some(task)=self.subscription.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take(){task.abort();}}}
