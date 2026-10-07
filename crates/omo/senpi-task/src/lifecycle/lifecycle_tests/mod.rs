//! Shared fakes for the lifecycle tests (`lifecycle/__fixtures__/lifecycle-fakes.ts`).

mod admission_lease;
mod batch_admission;
mod batch_admission_race;
mod destroy;
mod reconcile;
mod reconcile_multi_session;
mod reconcile_reattach;
mod reconcile_revival;
mod residency;
mod shutdown;
mod single_writer_audit;
mod ttl;

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use super::port::{
    OrphanSignal, ProcessSignaller, ResidencyRegistry, ResidentHandle, ResidentKind,
};
use super::settings::TaskSettings;
use super::store_port::{LifecycleStore, RecordMutation, RetainPredicate};
use super::{LifecycleDeps, TaskLifecycle, create_task_lifecycle};
use crate::host::HostError;
use crate::state::{ResidencyState, TaskNotification, TaskRecord, TaskStatus};
use crate::state::{TaskTransition, TaskTransitionResult};
use crate::store::{ListTaskRecordsResult, PersistedTaskEvent, StoreError, TombstoneResult};
use crate::store::{StateDirConfig, TaskRecordStore};

pub(crate) struct TempStore {
    pub store: Arc<TaskRecordStore>,
    _dir: tempfile::TempDir,
}

pub(crate) fn temp_store() -> TempStore {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TaskRecordStore::new(&StateDirConfig {
        project_dir: dir.path().to_path_buf(),
        task_state_dir: None,
    });
    TempStore {
        store: Arc::new(store),
        _dir: dir,
    }
}

pub(crate) fn settings(overrides: Value) -> TaskSettings {
    TaskSettings::resolve(&overrides).expect("valid task settings")
}

pub(crate) fn default_settings() -> TaskSettings {
    settings(json!({}))
}

#[derive(Default)]
pub(crate) struct Seed<'a> {
    pub task_id: &'a str,
    pub parent_session_id: Option<&'a str>,
    pub status: Option<TaskStatus>,
    pub residency_state: Option<ResidencyState>,
    pub execution_mode: Option<&'a str>,
    pub updated_at: Option<String>,
    pub pid: Option<i64>,
    pub child_session_id: Option<&'a str>,
    pub host_pid: Option<i64>,
    pub runner_kind: Option<crate::state::RunnerKind>,
    pub host_session: Option<crate::state::HostSessionIdentity>,
    pub killed: bool,
    pub run_epoch: Option<i64>,
    pub notify_on_terminal: bool,
    pub notified_epoch: Option<i64>,
    pub notification_failed_epoch: Option<i64>,
}

pub(crate) fn seed_record(store: &TaskRecordStore, input: Seed<'_>) -> TaskRecord {
    let timestamp = input
        .updated_at
        .clone()
        .unwrap_or_else(|| crate::shared::iso_from_ms(now_ms()));
    let parent = input.parent_session_id.unwrap_or("parent-1");
    let mut record = crate::test_support::base_record(input.task_id, parent);
    record.name = Some(input.task_id.to_string());
    record.execution_mode = input.execution_mode.unwrap_or("in-process").to_string();
    record.model = "anthropic/claude".to_string();
    record.status = input.status.unwrap_or(TaskStatus::Completed);
    record.residency_state = input.residency_state.unwrap_or(ResidencyState::Resident);
    record.created_at.clone_from(&timestamp);
    record.updated_at = timestamp;
    record.final_response = None;
    record.notify_on_terminal = input.notify_on_terminal;
    record.notification = TaskNotification {
        run_epoch: input.run_epoch.unwrap_or(0),
        notified_epoch: input.notified_epoch.unwrap_or(-1),
        notification_failed_epoch: input.notification_failed_epoch,
        ..TaskNotification::default()
    };
    record.killed = input.killed.then_some(true);
    record.pid = input.pid;
    record.child_session_id = input.child_session_id.map(str::to_string);
    record.host_pid = input.host_pid;
    record.runner_kind = input.runner_kind;
    record.host_session = input.host_session;
    store.save(&record).expect("seed record");
    record
}

pub(crate) fn now_ms() -> i64 {
    i64::try_from(crate::state::system_now_ms()).unwrap_or(i64::MAX)
}

/// `new Date(1_000_000 + offset).toISOString()`.
pub(crate) fn iso(offset_ms: i64) -> String {
    crate::shared::iso_from_ms(1_000_000 + offset_ms)
}

pub(crate) type CallLog = Arc<Mutex<Vec<String>>>;

pub(crate) fn call_log() -> CallLog {
    Arc::default()
}

pub(crate) fn calls(log: &CallLog) -> Vec<String> {
    log.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

#[derive(Default)]
pub(crate) struct HandleOptions {
    pub pid: Option<i64>,
    pub abort_rejects: bool,
    pub dispose_rejects: bool,
}

pub(crate) struct FakeHandle {
    task_id: String,
    kind: ResidentKind,
    order: CallLog,
    options: HandleOptions,
    steps: Mutex<HashSet<&'static str>>,
}

impl FakeHandle {
    fn step(&self, step: &'static str) {
        self.steps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(step);
        self.order
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("{step}:{}", self.task_id));
    }

    pub fn did(&self, step: &str) -> bool {
        self.steps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(step)
    }
}

pub(crate) fn fake_handle(
    task_id: &str,
    kind: ResidentKind,
    order: &CallLog,
    options: HandleOptions,
) -> Arc<FakeHandle> {
    Arc::new(FakeHandle {
        task_id: task_id.to_string(),
        kind,
        order: Arc::clone(order),
        options,
        steps: Mutex::default(),
    })
}

impl ResidentHandle for FakeHandle {
    fn task_id(&self) -> &str {
        &self.task_id
    }
    fn kind(&self) -> ResidentKind {
        self.kind
    }
    fn pid(&self) -> Option<i64> {
        self.options.pid
    }
    fn abort(&self) -> Result<(), HostError> {
        self.step("abort");
        if self.options.abort_rejects {
            return Err(HostError {
                message: "child already exited".to_string(),
            });
        }
        Ok(())
    }
    fn dispose(&self) -> Result<(), HostError> {
        self.step("dispose");
        if self.options.dispose_rejects {
            return Err(HostError {
                message: "dispose exploded".to_string(),
            });
        }
        Ok(())
    }
    fn terminate(&self) -> Result<(), HostError> {
        self.step("terminate");
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct FakeRegistry {
    handles: Mutex<BTreeMap<String, Arc<dyn ResidentHandle>>>,
    pending: Mutex<HashSet<String>>,
    forgotten: Mutex<Vec<String>>,
    order: Option<CallLog>,
}

impl FakeRegistry {
    /// `OrderRegistry`: also logs `forget:<id>` into the shared call order.
    pub fn with_order(order: &CallLog) -> Self {
        Self {
            order: Some(Arc::clone(order)),
            ..Self::default()
        }
    }
    pub fn add(&self, handle: Arc<dyn ResidentHandle>) {
        self.handles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(handle.task_id().to_string(), handle);
    }
    pub fn mark_pending(&self, task_id: &str) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(task_id.to_string());
    }
    pub fn forgotten(&self) -> Vec<String> {
        self.forgotten
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl ResidencyRegistry for FakeRegistry {
    fn get(&self, task_id: &str) -> Option<Arc<dyn ResidentHandle>> {
        self.handles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(task_id)
            .cloned()
    }
    fn entries(&self) -> Vec<Arc<dyn ResidentHandle>> {
        self.handles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }
    fn forget(&self, task_id: &str) {
        if let Some(order) = &self.order {
            order
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("forget:{task_id}"));
        }
        self.handles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(task_id);
        self.forgotten
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(task_id.to_string());
    }
    fn has_pending_sends(&self, task_id: &str) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(task_id)
    }
}

pub(crate) fn read_events(store: &TaskRecordStore, task_id: &str) -> Vec<String> {
    let path = store
        .state_dir()
        .join("logs")
        .join(format!("{task_id}.jsonl"));
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter(|line| !line.is_empty())
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter_map(|event| {
                    event
                        .get("type")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn lifecycle(
    store: &Arc<TaskRecordStore>,
    registry: &Arc<FakeRegistry>,
    config: TaskSettings,
) -> TaskLifecycle {
    create_task_lifecycle(LifecycleDeps::new(store.clone(), registry.clone(), config))
}

pub(crate) fn residency(store: &TaskRecordStore, task_id: &str) -> Option<ResidencyState> {
    store
        .load(task_id)
        .expect("load")
        .map(|record| record.residency_state)
}

pub(crate) type SignalHook = Box<dyn Fn(i64, OrphanSignal) + Send + Sync>;

/// `ProcessSignaller` over a mutable alive set; records every signal and runs an optional hook.
#[derive(Default)]
pub(crate) struct FakeSignaller {
    alive: Mutex<HashSet<i64>>,
    signals: Mutex<Vec<(i64, &'static str)>>,
    hook: Option<SignalHook>,
    /// When true, a SIGTERM kills the pid (the process honours TERM).
    pub dies_on_term: bool,
}

impl FakeSignaller {
    pub fn alive(pids: impl IntoIterator<Item = i64>) -> Arc<Self> {
        Arc::new(Self::with_alive(pids))
    }
    pub fn with_alive(pids: impl IntoIterator<Item = i64>) -> Self {
        Self {
            alive: Mutex::new(pids.into_iter().collect()),
            ..Self::default()
        }
    }
    pub fn with_hook(mut self, hook: SignalHook) -> Self {
        self.hook = Some(hook);
        self
    }
    pub fn signals(&self) -> Vec<(i64, &'static str)> {
        self.signals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    pub fn set_alive(&self, pid: i64, alive: bool) {
        let mut set = self.alive.lock().unwrap_or_else(PoisonError::into_inner);
        if alive {
            set.insert(pid);
        } else {
            set.remove(&pid);
        }
    }
}

impl ProcessSignaller for FakeSignaller {
    fn is_alive(&self, pid: i64) -> bool {
        self.alive
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&pid)
    }
    fn signal(&self, pid: i64, signal: OrphanSignal) {
        self.signals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((pid, signal.as_str()));
        if let Some(hook) = &self.hook {
            hook(pid, signal);
        }
        if self.dies_on_term {
            self.set_alive(pid, false);
        }
    }
}

pub(crate) type ListHook = Box<dyn FnMut(&TaskRecordStore) + Send>;

/// Delegates to a real store but runs `on_list` once, right after the first `list()` scan -
/// the TS tests' `{ ...store, list: () => ... }` race injection.
pub(crate) struct RacingStore {
    pub inner: Arc<TaskRecordStore>,
    on_list: Mutex<Option<ListHook>>,
}

impl RacingStore {
    pub fn new(inner: &Arc<TaskRecordStore>, on_list: ListHook) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::clone(inner),
            on_list: Mutex::new(Some(on_list)),
        })
    }
}

impl LifecycleStore for RacingStore {
    fn state_dir(&self) -> &std::path::Path {
        self.inner.state_dir()
    }
    fn load(&self, task_id: &str) -> Result<Option<TaskRecord>, StoreError> {
        self.inner.load(task_id)
    }
    fn list(&self) -> Result<ListTaskRecordsResult, StoreError> {
        let result = self.inner.list();
        let hook = self
            .on_list
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(mut hook) = hook {
            hook(&self.inner);
        }
        result
    }
    fn mutate(
        &self,
        task_id: &str,
        mutation: &mut RecordMutation<'_>,
    ) -> Result<Option<TaskRecord>, StoreError> {
        LifecycleStore::mutate(self.inner.as_ref(), task_id, mutation)
    }
    fn replace(&self, record: &TaskRecord) -> Result<(), StoreError> {
        self.inner.replace(record)
    }
    fn transition(
        &self,
        task_id: &str,
        transition: &TaskTransition,
    ) -> Result<TaskTransitionResult, StoreError> {
        self.inner.transition(task_id, transition)
    }
    fn append_event(&self, task_id: &str, event: &PersistedTaskEvent) -> Result<(), StoreError> {
        LifecycleStore::append_event(self.inner.as_ref(), task_id, event)
    }
    fn tombstone_if_expired(
        &self,
        task_id: &str,
        should_retain: &mut RetainPredicate<'_>,
    ) -> Result<TombstoneResult, StoreError> {
        LifecycleStore::tombstone_if_expired(self.inner.as_ref(), task_id, should_retain)
    }
    fn complete_expunge(&self, task_id: &str) -> Result<(), StoreError> {
        self.inner.complete_expunge(task_id)
    }
    fn list_expunging(&self) -> Result<Vec<String>, StoreError> {
        self.inner.list_expunging()
    }
}

pub(crate) fn record_path(store: &TaskRecordStore, task_id: &str) -> std::path::PathBuf {
    store
        .state_dir()
        .join("tasks")
        .join(format!("{task_id}.json"))
}

pub(crate) fn append_seed_event(store: &TaskRecordStore, task_id: &str) {
    store
        .append_event(
            task_id,
            &PersistedTaskEvent {
                event_type: "seed".to_string(),
                payload: json!({}),
            },
        )
        .expect("append seed event");
}
