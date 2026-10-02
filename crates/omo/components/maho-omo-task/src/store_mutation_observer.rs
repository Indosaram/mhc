use std::{ops::Deref,sync::Arc};
use senpi_task::{store::{TaskRecordStore,StoreError,TombstoneResult},state::{TaskRecord,TaskTransition,TaskTransitionResult}};
pub struct MutationNotifyingStore { backing:TaskRecordStore, on_mutation:Arc<dyn Fn()+Send+Sync> }
pub fn create_mutation_notifying_store(backing:TaskRecordStore,on_mutation:Arc<dyn Fn()+Send+Sync>)->MutationNotifyingStore { MutationNotifyingStore { backing,on_mutation } }
impl Deref for MutationNotifyingStore { type Target=TaskRecordStore; fn deref(&self)->&Self::Target { &self.backing } }
impl MutationNotifyingStore {
    pub fn save(&self,record:&TaskRecord)->Result<(),StoreError> { self.backing.save(record)?; (self.on_mutation)(); Ok(()) }
    pub fn replace(&self,record:&TaskRecord)->Result<(),StoreError> { self.backing.replace(record)?; (self.on_mutation)(); Ok(()) }
    pub fn mutate(&self,id:&str,mutation:impl FnOnce(&TaskRecord)->TaskRecord)->Result<Option<TaskRecord>,StoreError> { let result=self.backing.mutate(id,mutation)?; (self.on_mutation)(); Ok(result) }
    pub fn remove(&self,id:&str)->Result<(),StoreError> { self.backing.remove(id)?; (self.on_mutation)(); Ok(()) }
    pub fn transition(&self,id:&str,transition:&TaskTransition)->Result<TaskTransitionResult,StoreError> { let result=self.backing.transition(id,transition)?; (self.on_mutation)(); Ok(result) }
    pub fn tombstone_if_expired(&self,id:&str,retain:impl FnOnce(&TaskRecord)->bool)->Result<TombstoneResult,StoreError> { let result=self.backing.tombstone_if_expired(id,retain)?; if matches!(result,TombstoneResult::Tombstoned(_)) { (self.on_mutation)(); } Ok(result) }
    pub fn complete_expunge(&self,id:&str)->Result<(),StoreError> { self.backing.complete_expunge(id)?; (self.on_mutation)(); Ok(()) }
}
