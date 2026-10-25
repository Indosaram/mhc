use std::{ops::Deref,sync::Arc};
use senpi_task::{state::{TaskRecord,TaskStatus,TaskTransition,TaskTransitionResult},store::{StoreError,TaskRecordStore,TombstoneResult}};

/// One committed record mutation (`StoreMutation`): the task it belongs to, the record as it now
/// stands, and the status it replaced. A zero-argument notification cannot drive the status
/// spend ledger, which is why the record travels with every notification.
#[derive(Clone,Debug)]
pub struct StoreMutation { pub task_id:String, pub record:TaskRecord, pub previous_status:Option<TaskStatus> }

pub type MutationListener = Arc<dyn Fn(&StoreMutation) + Send + Sync>;

pub struct MutationNotifyingStore { backing:TaskRecordStore, on_mutation:MutationListener }

pub fn create_mutation_notifying_store(backing:TaskRecordStore,on_mutation:MutationListener)->MutationNotifyingStore { MutationNotifyingStore { backing,on_mutation } }

impl Deref for MutationNotifyingStore { type Target=TaskRecordStore; fn deref(&self)->&Self::Target { &self.backing } }

impl MutationNotifyingStore {
    fn notify(&self,record:&TaskRecord,previous_status:Option<TaskStatus>) {
        (self.on_mutation)(&StoreMutation { task_id:record.task_id.clone(),record:record.clone(),previous_status });
    }
    fn reload(&self,id:&str)->Option<TaskRecord> { self.backing.load(id).ok().flatten() }
    pub fn save(&self,record:&TaskRecord)->Result<(),StoreError> {
        let previous=self.reload(&record.task_id).map(|previous| previous.status);
        self.backing.save(record)?; self.notify(record,previous); Ok(())
    }
    pub fn replace(&self,record:&TaskRecord)->Result<(),StoreError> {
        let previous=self.reload(&record.task_id).map(|previous| previous.status);
        self.backing.replace(record)?; self.notify(record,previous); Ok(())
    }
    pub fn mutate(&self,id:&str,mutation:impl FnOnce(&TaskRecord)->TaskRecord)->Result<Option<TaskRecord>,StoreError> {
        let previous=self.reload(id).map(|previous| previous.status);
        let result=self.backing.mutate(id,mutation)?;
        if let Some(record)=&result { self.notify(record,previous); }
        Ok(result)
    }
    pub fn remove(&self,id:&str)->Result<(),StoreError> {
        let previous=self.reload(id);
        self.backing.remove(id)?;
        if let Some(record)=previous { (self.on_mutation)(&StoreMutation { task_id:record.task_id.clone(),record,previous_status:None }); }
        Ok(())
    }
    pub fn transition(&self,id:&str,transition:&TaskTransition)->Result<TaskTransitionResult,StoreError> {
        let previous=self.reload(id).map(|previous| previous.status);
        let result=self.backing.transition(id,transition)?;
        if let Some(record)=self.reload(id) { self.notify(&record,previous); }
        Ok(result)
    }
    pub fn tombstone_if_expired(&self,id:&str,retain:impl FnOnce(&TaskRecord)->bool)->Result<TombstoneResult,StoreError> {
        let previous=self.reload(id).map(|previous| previous.status);
        let result=self.backing.tombstone_if_expired(id,retain)?;
        if let TombstoneResult::Tombstoned(record)=&result { self.notify(record,previous); }
        Ok(result)
    }
    pub fn complete_expunge(&self,id:&str)->Result<(),StoreError> { self.backing.complete_expunge(id)?; Ok(()) }
}
