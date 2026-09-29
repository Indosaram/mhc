use std::sync::Arc;

use serde::Serialize;

use crate::host::HostError;
use crate::state::{ResolvedModelRecord, TaskRecord, TaskRunStats, TaskStatus};
use crate::store::{ListTaskRecordsResult, PersistedTaskEvent, StoreError};

pub const COMPLETION_CUSTOM_TYPE: &str = "senpi-task.completion";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionReason {
    Compacting,
    SessionSwitching,
    SessionShutdown,
}

/// The live parent-session state the completion push routes against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentState {
    Idle,
    Streaming,
    Compacting,
    SessionSwitching,
    SessionShutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingDecision {
    Wake,
    DeliverStreaming,
    Buffer(TransitionReason),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CompletionDetails {
    pub task_id: String,
    pub name: String,
    pub status: TaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<ResolvedModelRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_models: Option<Vec<ResolvedModelRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<ResolvedModelRecord>,
    pub duration_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_stats: Option<TaskRunStats>,
    pub final_response: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_response_file: Option<String>,
    pub continuation_hint: String,
}

/// One message carries one or many completions; `custom_type` is always
/// [`COMPLETION_CUSTOM_TYPE`].
#[derive(Debug, Clone, PartialEq)]
pub struct ParentNotifierMessage {
    pub custom_type: &'static str,
    pub content: String,
    pub display: bool,
    pub details: Vec<CompletionDetails>,
    pub trigger_turn: Option<bool>,
}

/// Synchronous enqueue seam into the parent session. A returned error is the only observable
/// delivery failure; delivery itself is fire-and-forget.
pub trait ParentNotifier: Send + Sync {
    fn enqueue(&self, message: &ParentNotifierMessage) -> Result<(), HostError>;
}

/// The record store as the notifier sees it. Notification bookkeeping goes through `mutate` (locked
/// re-read + conditional write) so a concurrent residency/host_pid claim is never erased.
pub trait CompletionNotifierStore: Send + Sync {
    fn load(&self, task_id: &str) -> Result<Option<TaskRecord>, StoreError>;
    fn list(&self) -> Result<ListTaskRecordsResult, StoreError>;
    fn replace(&self, record: &TaskRecord) -> Result<(), StoreError>;
    fn mutate(
        &self,
        task_id: &str,
        mutation: &mut dyn FnMut(&TaskRecord) -> TaskRecord,
    ) -> Result<Option<TaskRecord>, StoreError>;
    fn append_event(&self, task_id: &str, event: &PersistedTaskEvent)
    -> Result<String, StoreError>;
}

impl CompletionNotifierStore for crate::store::TaskRecordStore {
    fn load(&self, task_id: &str) -> Result<Option<TaskRecord>, StoreError> {
        Self::load(self, task_id)
    }
    fn list(&self) -> Result<ListTaskRecordsResult, StoreError> {
        Self::list(self)
    }
    fn replace(&self, record: &TaskRecord) -> Result<(), StoreError> {
        Self::replace(self, record)
    }
    fn mutate(
        &self,
        task_id: &str,
        mutation: &mut dyn FnMut(&TaskRecord) -> TaskRecord,
    ) -> Result<Option<TaskRecord>, StoreError> {
        Self::mutate(self, task_id, |record| mutation(record))
    }
    fn append_event(
        &self,
        task_id: &str,
        event: &PersistedTaskEvent,
    ) -> Result<String, StoreError> {
        Self::append_event(self, task_id, event).map(|path| path.to_string_lossy().into_owned())
    }
}

pub type ScheduledTask = Box<dyn FnOnce() + Send>;
pub type ScheduledCancel = Box<dyn FnOnce() + Send>;
/// Schedules `task` after `delay_ms`; the returned closure cancels it.
pub type CompletionRetrySchedule = Arc<dyn Fn(ScheduledTask, u64) -> ScheduledCancel + Send + Sync>;

pub struct CompletionNotifierDeps {
    pub notifier: Arc<dyn ParentNotifier>,
    pub store: Arc<dyn CompletionNotifierStore>,
    pub state_dir: Option<std::path::PathBuf>,
    pub schedule: Option<CompletionRetrySchedule>,
    pub get_parent_state: Option<Arc<dyn Fn() -> ParentState + Send + Sync>>,
    pub get_current_session_id: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
}

impl CompletionNotifierDeps {
    pub fn new(notifier: Arc<dyn ParentNotifier>, store: Arc<dyn CompletionNotifierStore>) -> Self {
        Self {
            notifier,
            store,
            state_dir: None,
            schedule: None,
            get_parent_state: None,
            get_current_session_id: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub record: TaskRecord,
    pub parent_state: ParentState,
    pub run_in_background: bool,
    pub tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    SyncTask,
    NonNotifyingTerminal,
    NotTerminal,
    AlreadyNotified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveredDecision {
    Wake,
    DeliverStreaming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyResult {
    Skipped(SkipReason),
    Delivered(DeliveredDecision),
    Buffered(TransitionReason),
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushInput {
    pub session_id: String,
    pub replaced: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileUnnotifiedNotificationsInput<'a> {
    pub session_id: &'a str,
    pub parent_state: ParentState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushResult {
    Flushed(usize),
    Dropped(usize),
    Failed(usize),
    Empty,
}
