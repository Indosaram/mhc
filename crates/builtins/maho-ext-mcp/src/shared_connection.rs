use std::{collections::BTreeMap,path::PathBuf,sync::{Arc,Mutex},time::Duration};
use crate::{connection::ServerConnection,errors::{McpError,McpErrorKind},catalog_cache::{McpCachedServerCatalog,collect_server_catalog_for_cache,write_mcp_cached_server},transport_sdk::McpClient};
struct SharedState {leases:BTreeMap<u64,usize>,calls:BTreeMap<u64,usize>,disposed:bool,in_flight:usize,idle:Option<tokio::task::JoinHandle<()>>}
pub struct SharedMcpConnection {
    pub connection:Arc<ServerConnection>,state:Mutex<SharedState>,catalog:tokio::sync::Mutex<Option<(u64,McpCachedServerCatalog)>>,
    renewal:tokio::sync::Mutex<()>,agent_dir:PathBuf,config_hash:String,request_timeout:Duration,
    subscription:Mutex<Option<tokio::task::JoinHandle<()>>>,
    idle_timeout:Duration,
}
impl SharedMcpConnection {
    pub fn new(connection:Arc<ServerConnection>,agent_dir:PathBuf,config_hash:String,request_timeout:Duration)->Arc<Self> {
        Self::with_idle_timeout(connection,agent_dir,config_hash,request_timeout,Duration::from_secs(600))
    }
    pub fn with_idle_timeout(connection:Arc<ServerConnection>,agent_dir:PathBuf,config_hash:String,request_timeout:Duration,idle_timeout:Duration)->Arc<Self> {
        let mut changes=connection.on_tools_changed();
        let shared=Arc::new(Self {connection,state:Mutex::new(SharedState {leases:BTreeMap::new(),calls:BTreeMap::new(),disposed:false,in_flight:0,idle:None}),catalog:tokio::sync::Mutex::new(None),renewal:tokio::sync::Mutex::new(()),agent_dir,config_hash,request_timeout,subscription:Mutex::new(None),idle_timeout});
        let weak=Arc::downgrade(&shared);let task=tokio::spawn(async move {loop {match changes.recv().await {Ok(_)|Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>{let Some(shared)=weak.upgrade() else{return;};*shared.catalog.lock().await=None;},Err(_)=>return}}});
        *shared.subscription.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=Some(task);shared
    }
    pub fn attach(self:&Arc<Self>,owner:u64,key:String)->Result<Arc<crate::shared_lease::SharedMcpLease>,McpError> {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.disposed{return Err(McpError::new(McpErrorKind::Connect,"MCP shared connection disposed"));}
        if let Some(timer)=state.idle.take(){timer.abort();}
        *state.leases.entry(owner).or_default()+=1;
        Ok(crate::shared_lease::SharedMcpLease::new(self.clone(),owner,key))
    }
    pub fn release(self:&Arc<Self>,owner:u64) {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(count)=state.leases.get_mut(&owner) {*count-=1;if *count==0 {state.leases.remove(&owner);}}
        drop(state);self.arm_idle();
    }
    fn arm_idle(self:&Arc<Self>) {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.disposed || !state.leases.is_empty() || state.in_flight>0 || state.idle.is_some(){return;}
        let deadline=tokio::time::Instant::now()+self.idle_timeout;let weak=Arc::downgrade(self);
        state.idle=Some(tokio::spawn(async move {tokio::time::sleep_until(deadline).await;let Some(shared)=weak.upgrade() else{return;};
            let dispose={let mut state=shared.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.idle=None;let dispose=!state.disposed && state.leases.is_empty() && state.in_flight==0;if dispose{state.disposed=true;}dispose};
            if dispose {
                if let Some(task)=shared.subscription.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take(){task.abort();}
                if let Err(error)=shared.connection.dispose().await {eprintln!("MCP shared idle disposal failed: {error}");}
            }
        }));
    }
    pub async fn run_request<T>(self:&Arc<Self>,call:impl std::future::Future<Output=T>)->T {
        {let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.in_flight+=1;if let Some(timer)=state.idle.take(){timer.abort();}}
        struct RequestGuard(Arc<SharedMcpConnection>);
        impl Drop for RequestGuard {fn drop(&mut self) {{let mut state=self.0.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.in_flight-=1;}self.0.arm_idle();}}
        let _guard=RequestGuard(self.clone());call.await
    }
    pub fn lease_count(&self)->usize {self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).leases.values().sum()}
    pub fn is_disposed(&self)->bool {self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).disposed}
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
        let _guard=CallGuard {shared:self.clone(),owner};self.run_request(call).await
    }
    pub async fn catalog(&self,refresh:bool)->Result<McpCachedServerCatalog,McpError> {
        let client=self.connect().await?;let mut cached=self.catalog.lock().await;let generation=self.connection.generation();
        if !refresh && let Some((previous,catalog))=&*cached && *previous==generation{return Ok(catalog.clone());}
        let catalog=collect_server_catalog_for_cache(&client,self.request_timeout,&self.config_hash).await?;
        if self.connection.generation()==generation {
            if cached.as_ref().is_none_or(|(previous,_)|*previous!=generation){write_mcp_cached_server(&self.agent_dir,&self.connection.server_name,catalog.clone()).map_err(|error|McpError::new(McpErrorKind::Connect,error.to_string()))?;}
            *cached=Some((generation,catalog.clone()));
            crate::resources::ensure_mcp_resource_subscriptions(client.clone(),&catalog.resources,self.request_timeout).await;
        }
        Ok(catalog)
    }
    pub async fn dispose(&self)->Result<(),McpError> {
        {let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if state.disposed{return Ok(());}state.disposed=true;state.leases.clear();if let Some(timer)=state.idle.take(){timer.abort();}}
        if let Some(task)=self.subscription.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take(){task.abort();}
        self.connection.dispose().await
    }
}
impl Drop for SharedMcpConnection {fn drop(&mut self){if let Some(task)=self.subscription.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take(){task.abort();}}}
