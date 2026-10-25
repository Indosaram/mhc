use senpi_task::{
    state::{TaskRecord, TaskStatus, TaskTransition, TaskTransitionResult},
    store::{StoreError, TaskRecordStore, TombstoneResult},
};
use std::{ops::Deref, sync::Arc};
#[derive(Clone, Debug)]
pub struct StoreMutation {
    pub task_id: String,
    pub record: Option<TaskRecord>,
    pub previous_status: Option<TaskStatus>,
}
pub type MutationListener = Arc<dyn Fn(&StoreMutation) + Send + Sync>;
pub struct MutationNotifyingStore {
    backing: TaskRecordStore,
    on_mutation: Arc<dyn Fn(StoreMutation) + Send + Sync>,
}
pub fn create_mutation_notifying_store(
    backing: TaskRecordStore,
    on_mutation: Arc<dyn Fn(StoreMutation) + Send + Sync>,
) -> MutationNotifyingStore {
    MutationNotifyingStore {
        backing,
        on_mutation,
    }
}
impl Deref for MutationNotifyingStore {
    type Target = TaskRecordStore;
    fn deref(&self) -> &Self::Target {
        &self.backing
    }
}
impl MutationNotifyingStore {
    fn notify(&self, id: &str, record: Option<TaskRecord>) {
        (self.on_mutation)(StoreMutation {
            task_id: id.into(),
            record,
            previous_status: None,
        });
    }
    pub fn save(&self, record: &TaskRecord) -> Result<(), StoreError> {
        self.backing.save(record)?;
        self.notify(&record.task_id, Some(record.clone()));
        Ok(())
    }
    pub fn replace(&self, record: &TaskRecord) -> Result<(), StoreError> {
        self.backing.replace(record)?;
        self.notify(&record.task_id, Some(record.clone()));
        Ok(())
    }
    pub fn mutate(
        &self,
        id: &str,
        mutation: impl FnOnce(&TaskRecord) -> TaskRecord,
    ) -> Result<Option<TaskRecord>, StoreError> {
        let result = self.backing.mutate(id, mutation)?;
        self.notify(id, result.clone());
        Ok(result)
    }
    pub fn remove(&self, id: &str) -> Result<(), StoreError> {
        self.backing.remove(id)?;
        self.notify(id, None);
        Ok(())
    }
    pub fn transition(
        &self,
        id: &str,
        transition: &TaskTransition,
    ) -> Result<TaskTransitionResult, StoreError> {
        let result = self.backing.transition(id, transition)?;
        let record = if result.applied {
            Some(result.record.clone())
        } else {
            self.backing.load(id)?
        };
        self.notify(id, record);
        Ok(result)
    }
    pub fn tombstone_if_expired(
        &self,
        id: &str,
        retain: impl FnOnce(&TaskRecord) -> bool,
    ) -> Result<TombstoneResult, StoreError> {
        let result = self.backing.tombstone_if_expired(id, retain)?;
        if matches!(result, TombstoneResult::Tombstoned(_)) {
            self.notify(id, None);
        }
        Ok(result)
    }
    pub fn restore_expunging(&self, id: &str) -> Result<(), StoreError> {
        self.backing.restore_expunging(id)?;
        self.notify(id, self.backing.load(id)?);
        Ok(())
    }
    pub fn complete_expunge(&self, id: &str) -> Result<(), StoreError> {
        self.backing.complete_expunge(id)?;
        self.notify(id, None);
        Ok(())
    }
}
