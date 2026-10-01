use std::{collections::BTreeMap,sync::{Arc,Mutex}};
use crate::{connection::ServerConnection,errors::McpError};
pub use crate::sharing_policy::shareable;
#[derive(Debug,thiserror::Error)]
#[error("Cannot detach an unknown MCP connection owner")]
pub struct HostMcpRegistryError {pub code:&'static str,pub key:String}
#[derive(Debug,thiserror::Error)]
pub enum RegistryDetachError {#[error(transparent)] Owner(#[from] HostMcpRegistryError),#[error(transparent)] Connection(#[from] McpError)}
struct RegistryEntry {connection:Arc<ServerConnection>,shareable:bool,owners:BTreeMap<u64,usize>}
#[derive(Default)]
pub struct HostMcpRegistry {entries:Mutex<BTreeMap<String,Vec<RegistryEntry>>>,shared:Mutex<BTreeMap<String,Arc<crate::shared_connection::SharedMcpConnection>>>}
impl HostMcpRegistry {
    pub fn attach_shared(&self,key:&str,owner:u64,factory:impl FnOnce()->Arc<crate::shared_connection::SharedMcpConnection>)->Result<Arc<crate::shared_lease::SharedMcpLease>,McpError> {
        let mut shared=self.shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);let connection=shared.entry(key.into()).or_insert_with(factory);connection.attach(owner,key.into())
    }
    pub fn attach(&self,key:&str,owner:u64,factory:impl FnOnce()->Arc<ServerConnection>,can_share:bool)->Arc<ServerConnection> {
        let mut entries=self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let entries=entries.entry(key.into()).or_default();
        if let Some(entry)=entries.iter_mut().find(|entry|entry.owners.contains_key(&owner)) {
            if let Some(count)=entry.owners.get_mut(&owner){*count+=1;}
            return entry.connection.clone();
        }
        if can_share && let Some(entry)=entries.iter_mut().find(|entry|entry.shareable) {entry.owners.insert(owner,1);return entry.connection.clone();}
        let connection=factory();entries.push(RegistryEntry {connection:connection.clone(),shareable:can_share,owners:BTreeMap::from([(owner,1)])});connection
    }
    pub async fn detach(&self,key:&str,owner:u64)->Result<(),RegistryDetachError> {
        let connection={
            let mut all=self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let error=||HostMcpRegistryError {code:"unknown_owner",key:key.into()};
            let entries=all.get_mut(key).ok_or_else(error)?;
            let index=entries.iter().position(|entry|entry.owners.contains_key(&owner)).ok_or_else(error)?;
            let entry=&mut entries[index];
            if let Some(count)=entry.owners.get_mut(&owner) && *count>1 {*count-=1;return Ok(());}
            entry.owners.remove(&owner);if !entry.owners.is_empty(){return Ok(());}
            let connection=entries.remove(index).connection;
            if entries.is_empty(){all.remove(key);}connection
        };
        connection.dispose().await?;Ok(())
    }
    pub fn for_each_owner(&self,key:&str)->Vec<u64> {
        self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(key).into_iter().flatten().flat_map(|entry|entry.owners.keys().copied()).collect()
    }
    pub async fn dispose(&self)->Result<(),McpError> {
        let shared=std::mem::take(&mut *self.shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        futures::future::try_join_all(shared.values().map(|connection|connection.dispose())).await?;
        let entries=std::mem::take(&mut *self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        let connections=entries.into_values().flatten().map(|entry|entry.connection).collect::<Vec<_>>();
        futures::future::try_join_all(connections.iter().map(|connection|connection.dispose())).await?;Ok(())
    }
    pub fn size(&self)->usize {self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).values().map(Vec::len).sum()}
}
