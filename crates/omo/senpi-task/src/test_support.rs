//! Shared in-crate test fixtures for slice-B modules.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::completion::CompletionNotifierStore;
use crate::state::{ResidencyState, TaskNotification, TaskRecord, TaskStatus};
use crate::store::{ListTaskRecordsResult, PersistedTaskEvent, StoreError};

/// A completed, resident record mirroring the TypeScript tests' `baseRecord()` literal.
pub(crate) fn base_record(task_id: &str, parent_session_id: &str) -> TaskRecord {
    TaskRecord {
        task_id: task_id.to_string(),
        status: TaskStatus::Completed,
        residency_state: ResidencyState::Resident,
        parent_session_id: parent_session_id.to_string(),
        root_session_id: parent_session_id.to_string(),
        depth: 1,
        execution_mode: "in-process".to_string(),
        model: "gpt-5.2".to_string(),
        notify_on_terminal: false,
        created_at: "2026-07-06T01:00:00.000Z".to_string(),
        updated_at: "2026-07-06T01:00:03.000Z".to_string(),
        notification: TaskNotification::default(),
        name: None,
        task_summary: None,
        description: None,
        agent_type: None,
        category: None,
        tool_allow: None,
        tool_deny: None,
        requested_model: None,
        fallback_models: None,
        fallback_attempts: None,
        resolved_model: None,
        spawn_spec: None,
        owner: None,
        pending_steering: None,
        pid: None,
        host_pid: None,
        child_session_id: None,
        final_response: Some("done".to_string()),
        error_message: None,
        killed: None,
        run_stats: None,
    }
}

type ClaimFn = Box<dyn Fn(&mut TaskRecord) + Send + Sync>;

/// In-memory store; `claim_on_persist` models a sibling writer landing between read and write.
#[derive(Default)]
pub(crate) struct MemoryStore {
    pub records: Mutex<BTreeMap<String, TaskRecord>>,
    pub mutated: Mutex<Vec<TaskRecord>>,
    pub events: Mutex<Vec<(String, PersistedTaskEvent)>>,
    pub claim_on_persist: Option<ClaimFn>,
}

impl MemoryStore {
    pub fn with(records: impl IntoIterator<Item = TaskRecord>) -> Arc<Self> {
        Arc::new(Self::seeded(records))
    }

    pub fn seeded(records: impl IntoIterator<Item = TaskRecord>) -> Self {
        let store = Self::default();
        for record in records {
            store.set(record);
        }
        store
    }

    pub fn get(&self, task_id: &str) -> Option<TaskRecord> {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(task_id)
            .cloned()
    }

    pub fn set(&self, record: TaskRecord) {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(record.task_id.clone(), record);
    }

    pub fn mutated(&self) -> Vec<TaskRecord> {
        self.mutated
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn events(&self) -> Vec<(String, PersistedTaskEvent)> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn apply_claim(&self, task_id: &str) {
        let Some(claim) = &self.claim_on_persist else {
            return;
        };
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(record) = records.get_mut(task_id) {
            claim(record);
        }
    }
}

impl CompletionNotifierStore for MemoryStore {
    fn load(&self, task_id: &str) -> Result<Option<TaskRecord>, StoreError> {
        Ok(self.get(task_id))
    }

    fn list(&self) -> Result<ListTaskRecordsResult, StoreError> {
        let records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(ListTaskRecordsResult {
            records: records.values().cloned().collect(),
            diagnostics: Vec::new(),
        })
    }

    fn replace(&self, record: &TaskRecord) -> Result<(), StoreError> {
        self.apply_claim(&record.task_id);
        self.set(record.clone());
        Ok(())
    }

    fn mutate(
        &self,
        task_id: &str,
        mutation: &mut dyn FnMut(&TaskRecord) -> TaskRecord,
    ) -> Result<Option<TaskRecord>, StoreError> {
        if self.get(task_id).is_none() {
            return Ok(None);
        }
        self.apply_claim(task_id);
        let Some(fresh) = self.get(task_id) else {
            return Ok(None);
        };
        let next = mutation(&fresh);
        if next != fresh {
            self.set(next.clone());
            self.mutated
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(next.clone());
        }
        Ok(Some(next))
    }

    fn append_event(
        &self,
        task_id: &str,
        event: &PersistedTaskEvent,
    ) -> Result<String, StoreError> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((task_id.to_string(), event.clone()));
        Ok(format!("{task_id}.jsonl"))
    }
}
