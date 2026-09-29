//! The store surface the lifecycle uses; implemented by [`TaskRecordStore`] and by test wrappers
//! that model racing writers.

use std::path::Path;

use crate::state::{TaskRecord, TaskTransition, TaskTransitionResult};
use crate::store::{
    ListTaskRecordsResult, PersistedTaskEvent, StoreError, TaskRecordStore, TombstoneResult,
};

pub type RecordMutation<'a> = dyn FnMut(&TaskRecord) -> TaskRecord + 'a;
pub type RetainPredicate<'a> = dyn FnMut(&TaskRecord) -> bool + 'a;

pub trait LifecycleStore: Send + Sync {
    fn state_dir(&self) -> &Path;
    fn load(&self, task_id: &str) -> Result<Option<TaskRecord>, StoreError>;
    fn list(&self) -> Result<ListTaskRecordsResult, StoreError>;
    fn mutate(
        &self,
        task_id: &str,
        mutation: &mut RecordMutation<'_>,
    ) -> Result<Option<TaskRecord>, StoreError>;
    fn replace(&self, record: &TaskRecord) -> Result<(), StoreError>;
    fn transition(
        &self,
        task_id: &str,
        transition: &TaskTransition,
    ) -> Result<TaskTransitionResult, StoreError>;
    fn append_event(&self, task_id: &str, event: &PersistedTaskEvent) -> Result<(), StoreError>;
    fn tombstone_if_expired(
        &self,
        task_id: &str,
        should_retain: &mut RetainPredicate<'_>,
    ) -> Result<TombstoneResult, StoreError>;
    fn complete_expunge(&self, task_id: &str) -> Result<(), StoreError>;
    fn list_expunging(&self) -> Result<Vec<String>, StoreError>;
}

impl LifecycleStore for TaskRecordStore {
    fn state_dir(&self) -> &Path {
        TaskRecordStore::state_dir(self)
    }

    fn load(&self, task_id: &str) -> Result<Option<TaskRecord>, StoreError> {
        TaskRecordStore::load(self, task_id)
    }

    fn list(&self) -> Result<ListTaskRecordsResult, StoreError> {
        TaskRecordStore::list(self)
    }

    fn mutate(
        &self,
        task_id: &str,
        mutation: &mut RecordMutation<'_>,
    ) -> Result<Option<TaskRecord>, StoreError> {
        TaskRecordStore::mutate(self, task_id, |record| mutation(record))
    }

    fn replace(&self, record: &TaskRecord) -> Result<(), StoreError> {
        TaskRecordStore::replace(self, record)
    }

    fn transition(
        &self,
        task_id: &str,
        transition: &TaskTransition,
    ) -> Result<TaskTransitionResult, StoreError> {
        TaskRecordStore::transition(self, task_id, transition)
    }

    fn append_event(&self, task_id: &str, event: &PersistedTaskEvent) -> Result<(), StoreError> {
        TaskRecordStore::append_event(self, task_id, event).map(drop)
    }

    fn tombstone_if_expired(
        &self,
        task_id: &str,
        should_retain: &mut RetainPredicate<'_>,
    ) -> Result<TombstoneResult, StoreError> {
        TaskRecordStore::tombstone_if_expired(self, task_id, |record| should_retain(record))
    }

    fn complete_expunge(&self, task_id: &str) -> Result<(), StoreError> {
        TaskRecordStore::complete_expunge(self, task_id)
    }

    fn list_expunging(&self) -> Result<Vec<String>, StoreError> {
        TaskRecordStore::list_expunging(self)
    }
}
