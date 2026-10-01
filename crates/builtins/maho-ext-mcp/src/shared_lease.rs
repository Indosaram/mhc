use std::{sync::{Arc,atomic::{AtomicBool,AtomicU64,Ordering}},time::Duration};
use serde_json::Value;
use crate::{shared_connection::SharedMcpConnection,connection::ServerConnectionState,errors::{McpError,McpErrorKind}};
pub struct SharedMcpLease {pub owner:u64,pub key:String,pub shared:Arc<SharedMcpConnection>,released:AtomicBool,local_generation:AtomicU64,refresh_catalog:AtomicBool}
impl SharedMcpLease {
    pub(crate) fn new(shared:Arc<SharedMcpConnection>,owner:u64,key:String)->Arc<Self> {Arc::new(Self {owner,key,shared,released:AtomicBool::new(false),local_generation:AtomicU64::new(0),refresh_catalog:AtomicBool::new(false)})}
    fn assert_attached(&self)->Result<(),McpError> {if self.released.load(Ordering::Acquire) || !self.shared.owner_attached(self.owner){Err(McpError::new(McpErrorKind::Connect,"MCP connection owner detached"))}else{Ok(())}}
    pub fn state(&self)->ServerConnectionState {if self.assert_attached().is_err(){ServerConnectionState::Disabled}else{self.shared.connection.state()}}
    pub fn generation(&self)->u64 {self.shared.connection.generation()+self.local_generation.load(Ordering::Acquire)}
    pub fn last_error(&self)->Option<McpError>{self.shared.connection.last_error()}
    pub async fn connect(&self)->Result<(),McpError>{self.assert_attached()?;self.shared.connect().await?;Ok(())}
    pub fn bump_generation(&self)->Result<(),McpError>{self.assert_attached()?;self.local_generation.fetch_add(1,Ordering::AcqRel);self.refresh_catalog.store(true,Ordering::Release);Ok(())}
    pub async fn renew(&self)->Result<(),McpError>{self.bump_generation()?;self.connect().await}
    pub async fn request(&self,method:&str,params:Value,timeout:Duration)->Result<Value,McpError> {
        self.assert_attached()?;let client=self.shared.connect().await?;
        if method=="tools/call"{self.shared.call_tool(self.owner,client.request(method,params,timeout)).await}else{client.request(method,params,timeout).await}
    }
    pub async fn catalog(&self)->Result<crate::catalog_cache::McpCachedServerCatalog,McpError>{self.assert_attached()?;self.shared.catalog(self.refresh_catalog.swap(false,Ordering::AcqRel)).await}
    pub fn dispose(&self){if !self.released.swap(true,Ordering::AcqRel){self.shared.release(self.owner);}}
}
impl Drop for SharedMcpLease {fn drop(&mut self){self.dispose();}}
