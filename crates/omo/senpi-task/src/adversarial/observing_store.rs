//! Rust port of `__adversarial__/observing-store.ts`: wraps a real store so notification
//! persistence (invariant 1) and every status write (invariant 2) are observed without changing
//! behaviour. Each method delegates straight to the backing store.
//!
//! The TS source is test-only harness code, so the module is compiled only for tests.
#![cfg(test)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::Value;

use crate::state::{TaskRecord, TaskStatus, TaskTransitionResult, is_spawn_spec_v1};
use crate::store::{ListTaskRecordsResult, StoreError, TaskRecordStore};

const TERMINAL: [&str; 5] = ["completed", "error", "cancelled", "interrupted", "lost"];
const STATUS_CHANGING: [&str; 6] = ["start", "complete", "fail", "cancel", "interrupt", "lose"];

pub fn is_terminal_status(status: TaskStatus) -> bool {
    TERMINAL.contains(&status.as_str())
}

/// One recorded breach of invariant 1 (exactly-once) or invariant 2 (terminal idempotence) seen
/// at the store seam, carrying enough context to name the offending record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreBreach {
    pub invariant: u8,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreObservations {
    /// `{task_id}:{epoch}` -> number of times a notification for that (task, epoch) was persisted.
    pub notify_commits: BTreeMap<String, u64>,
    /// `{task_id}:{epoch}` -> number of times that (task, epoch) was actually enqueued to the parent.
    pub enqueue_by_epoch: BTreeMap<String, u64>,
    pub breaches: Vec<StoreBreach>,
    pub lifecycle_breaches: Vec<String>,
    pub max_residents_by_parent: BTreeMap<String, usize>,
    pub forbidden_residency: BTreeSet<String>,
}

pub type SharedObservations = Arc<Mutex<StoreObservations>>;

pub fn lock_observations(observations: &SharedObservations) -> MutexGuard<'_, StoreObservations> {
    observations.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
pub struct ObservingStore {
    backing: TaskRecordStore,
    pub observations: SharedObservations,
}

pub fn create_observing_store(
    backing: TaskRecordStore,
    shared: Option<SharedObservations>,
) -> ObservingStore {
    ObservingStore {
        backing,
        observations: shared.unwrap_or_default(),
    }
}

impl ObservingStore {
    /// The wrapped store, for delegating methods that carry no observation.
    pub fn backing(&self) -> &TaskRecordStore {
        &self.backing
    }

    pub fn state_dir(&self) -> &Path {
        self.backing.state_dir()
    }

    fn peek(&self, task_id: &str) -> Option<TaskRecord> {
        self.backing.load(task_id).ok().flatten()
    }

    fn observe_write(&self, before: Option<&TaskRecord>, after: Option<&TaskRecord>) {
        let mut observations = lock_observations(&self.observations);
        if let Some(after) = after {
            observe_lifecycle_write(&self.backing, &mut observations, before, after);
        }
        observe_resident_counts(&self.backing, &mut observations);
    }

    pub fn save(&self, record: &TaskRecord) -> Result<(), StoreError> {
        let before = self.peek(&record.task_id);
        self.backing.save(record)?;
        let after = self.peek(&record.task_id);
        self.observe_write(before.as_ref(), after.as_ref());
        Ok(())
    }

    /// Runs a backing-store mutation for `task_id` and observes the resulting write.
    pub fn mutate_with<R, F>(&self, task_id: &str, mutation: F) -> R
    where
        F: FnOnce(&TaskRecordStore) -> R,
    {
        let before = self.peek(task_id);
        let result = mutation(&self.backing);
        let after = self.peek(task_id);
        self.observe_write(before.as_ref(), after.as_ref());
        result
    }

    pub fn load(&self, task_id: &str) -> Result<Option<TaskRecord>, StoreError> {
        self.backing.load(task_id)
    }

    pub fn list(&self) -> Result<ListTaskRecordsResult, StoreError> {
        self.backing.list()
    }

    pub fn remove(&self, task_id: &str) -> Result<(), StoreError> {
        self.backing.remove(task_id)?;
        let mut observations = lock_observations(&self.observations);
        observe_resident_counts(&self.backing, &mut observations);
        Ok(())
    }

    pub fn complete_expunge(&self, task_id: &str) -> Result<(), StoreError> {
        self.backing.complete_expunge(task_id)
    }

    pub fn list_expunging(&self) -> Result<Vec<String>, StoreError> {
        self.backing.list_expunging()
    }

    pub fn replace(&self, record: &TaskRecord) -> Result<(), StoreError> {
        let before = self.peek(&record.task_id);
        if let Some(previous) = before.as_ref() {
            let mut observations = lock_observations(&self.observations);
            observe_replace(&mut observations, previous, record);
        }
        self.backing.replace(record)?;
        let after = self.peek(&record.task_id);
        self.observe_write(before.as_ref(), after.as_ref());
        Ok(())
    }

    /// Runs a backing-store transition of kind `transition_type` (the TS `transition.type`) for
    /// `task_id`, observing terminal idempotence and the resulting write.
    pub fn transition_with<F>(
        &self,
        task_id: &str,
        transition_type: &str,
        transition: F,
    ) -> Result<TaskTransitionResult, StoreError>
    where
        F: FnOnce(&TaskRecordStore) -> Result<TaskTransitionResult, StoreError>,
    {
        let before = self.peek(task_id);
        let result = transition(&self.backing)?;
        {
            let mut observations = lock_observations(&self.observations);
            observe_transition(&mut observations, transition_type, before.as_ref(), &result);
        }
        let after = self.peek(task_id);
        self.observe_write(before.as_ref(), after.as_ref());
        Ok(result)
    }
}

/// Runs the same before/after write observation [`ObservingStore::save`] and [`ObservingStore::mutate_with`]
/// perform, for a caller that had to write through the manager- or lifecycle-owned concrete
/// `TaskRecordStore` (which cannot itself be substituted with `ObservingStore`) instead of through
/// this wrapper directly.
pub fn observe_store_write(
    backing: &TaskRecordStore,
    observations: &SharedObservations,
    before: Option<&TaskRecord>,
    after: Option<&TaskRecord>,
) {
    let mut guard = lock_observations(observations);
    if let Some(after) = after {
        observe_lifecycle_write(backing, &mut guard, before, after);
    }
    observe_resident_counts(backing, &mut guard);
}

/// Runs the same terminal-idempotence and notify-commit observation [`ObservingStore::replace`]
/// performs, for a caller that had to write through the manager- or lifecycle-owned concrete
/// `TaskRecordStore` directly.
pub fn observe_store_replace(observations: &SharedObservations, previous: &TaskRecord, record: &TaskRecord) {
    observe_replace(&mut lock_observations(observations), previous, record);
}

/// The full `replace` observation [`ObservingStore::replace`] performs: the notify-commit /
/// terminal-overwrite checks, then the lifecycle-write and resident-count observation against the
/// post-write record.
pub fn observe_store_replace_with_write(
    backing: &TaskRecordStore,
    observations: &SharedObservations,
    previous: &TaskRecord,
    record: &TaskRecord,
) {
    observe_store_replace(observations, previous, record);
    observe_store_write(backing, observations, Some(previous), Some(record));
}

fn observe_replace(observations: &mut StoreObservations, previous: &TaskRecord, record: &TaskRecord) {
    let next_notified = record.notification.notified_epoch;
    if next_notified > previous.notification.notified_epoch {
        let key = format!("{}:{}", record.task_id, next_notified);
        *observations.notify_commits.entry(key).or_insert(0) += 1;
    }
    if is_terminal_status(previous.status)
        && record.status != previous.status
        && record.notification.run_epoch <= previous.notification.run_epoch
    {
        observations.breaches.push(StoreBreach {
            invariant: 2,
            detail: format!(
                "replace overwrote terminal {} -> {} for {} at epoch {}",
                previous.status.as_str(),
                record.status.as_str(),
                record.task_id,
                record.notification.run_epoch
            ),
        });
    }
}

fn is_killed(record: &TaskRecord) -> bool {
    serde_json::to_value(record)
        .ok()
        .and_then(|value| value.get("killed").and_then(Value::as_bool))
        == Some(true)
}

fn observe_lifecycle_write(
    backing: &TaskRecordStore,
    observations: &mut StoreObservations,
    before: Option<&TaskRecord>,
    after: &TaskRecord,
) {
    let residency = after.residency_state.as_str();
    if let Some(before) = before {
        if after.notification.run_epoch < before.notification.run_epoch {
            observations.lifecycle_breaches.push(format!(
                "run_epoch regressed for {}: {} -> {}",
                after.task_id, before.notification.run_epoch, after.notification.run_epoch
            ));
        }
        let became_suspended = residency == "persisted_only" || residency == "rpc_detached";
        if became_suspended && after.notification.run_epoch != before.notification.run_epoch {
            observations.lifecycle_breaches.push(format!(
                "suspension changed run_epoch for {}: {} -> {}",
                after.task_id, before.notification.run_epoch, after.notification.run_epoch
            ));
        }
    }
    if after.status.as_str() == "lost" && has_recovery_artifact(backing, after) {
        observations
            .lifecycle_breaches
            .push(format!("recoverable task {} transitioned to lost", after.task_id));
    }
    if is_killed(after)
        || after.status.as_str() == "cancelled"
        || residency == "evicted"
        || residency == "disposed"
    {
        observations.forbidden_residency.insert(after.task_id.clone());
    } else if residency == "resident" && observations.forbidden_residency.contains(&after.task_id) {
        observations.lifecycle_breaches.push(format!(
            "deliberately stopped task {} regained residency",
            after.task_id
        ));
    }
}

fn has_recovery_artifact(backing: &TaskRecordStore, record: &TaskRecord) -> bool {
    if record.spawn_spec.as_ref().is_some_and(is_spawn_spec_v1) {
        return true;
    }
    let sessions = backing
        .state_dir()
        .join("children")
        .join(&record.task_id)
        .join("sessions");
    let Ok(entries) = std::fs::read_dir(&sessions) else {
        return false;
    };
    entries
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().ends_with(".jsonl"))
}

fn observe_resident_counts(backing: &TaskRecordStore, observations: &mut StoreObservations) {
    let Ok(listed) = backing.list() else {
        return;
    };
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for record in &listed.records {
        if record.residency_state.as_str() != "resident" {
            continue;
        }
        *counts.entry(record.parent_session_id.clone()).or_insert(0) += 1;
    }
    for (parent, count) in counts {
        let entry = observations.max_residents_by_parent.entry(parent).or_insert(0);
        *entry = (*entry).max(count);
    }
}

fn observe_transition(
    observations: &mut StoreObservations,
    transition_type: &str,
    before: Option<&TaskRecord>,
    result: &TaskTransitionResult,
) {
    let Some(before) = before else {
        return;
    };
    if !is_terminal_status(before.status) || !STATUS_CHANGING.contains(&transition_type) {
        return;
    }
    // A status-changing transition landing on an already-terminal record must be rejected AND
    // logged as a late transition (not silently dropped, not applied).
    let audit_type = result.audit.type_name();
    if result.applied || audit_type != "late_transition_ignored" {
        observations.breaches.push(StoreBreach {
            invariant: 2,
            detail: format!(
                "late {} on terminal {} for {} was {} (audit={})",
                transition_type,
                before.status.as_str(),
                before.task_id,
                if result.applied { "applied" } else { "not logged as late" },
                audit_type
            ),
        });
    }
}

mod tests {
    use std::sync::Arc;

    use pretty_assertions::assert_eq;

    use super::{
        ObservingStore, SharedObservations, StoreObservations, create_observing_store,
        is_terminal_status, lock_observations,
    };
    use crate::state::{TaskStatus, TaskTransitionResult};
    use crate::store::{StateDirConfig, StoreError, TaskRecordStore};

    fn store_in(dir: &std::path::Path) -> TaskRecordStore {
        TaskRecordStore::new(&StateDirConfig {
            project_dir: dir.to_path_buf(),
            task_state_dir: None,
        })
    }

    #[test]
    fn terminal_statuses_match_ts_set() {
        for name in ["completed", "error", "cancelled", "interrupted", "lost"] {
            let status = TaskStatus::parse(name).expect("known status");
            assert!(is_terminal_status(status), "{name} should be terminal");
        }
    }

    #[test]
    fn shared_observations_are_reused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let shared: SharedObservations = Arc::default();
        let observing = create_observing_store(store_in(dir.path()), Some(Arc::clone(&shared)));
        assert!(Arc::ptr_eq(&observing.observations, &shared));
        assert_eq!(*lock_observations(&shared), StoreObservations::default());
        assert_eq!(observing.state_dir(), observing.backing().state_dir());
    }

    #[test]
    fn delegating_methods_are_wired() {
        let dir = tempfile::tempdir().expect("tempdir");
        let observing = create_observing_store(store_in(dir.path()), None);
        let _ = observing.list();
        let _ = observing.list_expunging();
        let _save = ObservingStore::save;
        let _load = ObservingStore::load;
        let _remove = ObservingStore::remove;
        let _replace = ObservingStore::replace;
        let _expunge = ObservingStore::complete_expunge;
        let _mutate = ObservingStore::mutate_with::<(), fn(&TaskRecordStore)>;
        let _transition = ObservingStore::transition_with::<
            fn(&TaskRecordStore) -> Result<TaskTransitionResult, StoreError>,
        >;
        assert!(lock_observations(&observing.observations).breaches.is_empty());
    }
}
