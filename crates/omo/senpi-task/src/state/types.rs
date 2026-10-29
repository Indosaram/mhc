use serde::{Deserialize, Serialize, Serializer};

use crate::shared::DagTaskOwner;

use super::isolation::{IsolationRecord, TaskIsolationSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Paused,
    Completed,
    Error,
    Cancelled,
    Interrupted,
    Lost,
}

pub const TASK_STATUSES: [TaskStatus; 8] = [
    TaskStatus::Pending,
    TaskStatus::Running,
    TaskStatus::Paused,
    TaskStatus::Completed,
    TaskStatus::Error,
    TaskStatus::Cancelled,
    TaskStatus::Interrupted,
    TaskStatus::Lost,
];

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Lost => "lost",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        TASK_STATUSES
            .into_iter()
            .find(|status| status.as_str() == value)
    }

    pub fn is_terminal(self) -> bool {
        match self {
            Self::Pending | Self::Running | Self::Paused => false,
            Self::Completed | Self::Error | Self::Cancelled | Self::Interrupted | Self::Lost => {
                true
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidencyState {
    Resident,
    Evicted,
    Disposed,
    PersistedOnly,
    RpcDetached,
}

pub const RESIDENCY_STATES: [ResidencyState; 5] = [
    ResidencyState::Resident,
    ResidencyState::Evicted,
    ResidencyState::Disposed,
    ResidencyState::PersistedOnly,
    ResidencyState::RpcDetached,
];

impl ResidencyState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resident => "resident",
            Self::Evicted => "evicted",
            Self::Disposed => "disposed",
            Self::PersistedOnly => "persisted_only",
            Self::RpcDetached => "rpc_detached",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        RESIDENCY_STATES
            .into_iter()
            .find(|state| state.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Messageability {
    Steer,
    Revive,
    NotContinuable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedModelSource {
    Category,
    Explicit,
    Agent,
}

pub const RESOLVED_MODEL_SOURCES: [ResolvedModelSource; 3] = [
    ResolvedModelSource::Category,
    ResolvedModelSource::Explicit,
    ResolvedModelSource::Agent,
];

impl ResolvedModelSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Category => "category",
            Self::Explicit => "explicit",
            Self::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedModelRecord {
    pub provider: String,
    pub model_id: String,
    pub display: String,
    pub source: ResolvedModelSource,
    /// Deprecated mirror of `reasoning` during the unification window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// Deprecated legacy persisted spelling; read through `reasoning`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveModelSource {
    Category,
    Explicit,
    Agent,
    Set,
    Cycle,
    Restore,
    Fallback,
    FallbackRevert,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveModelRecord {
    pub provider: String,
    pub model_id: String,
    pub display: String,
    pub source: EffectiveModelSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub switched_by_operator: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuspensionReason {
    DaemonUnavailable,
    HandoffParked,
    HostDraining,
    HostIncompatible,
    IdleEvicted,
    OwnHostUnreachable,
    RevivalDeferred,
    SessionQuit,
    StoreIndexUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwaitingInputReason {
    ResumedTurnAborted,
    ResumedTurnUnfinished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAwaitingInput {
    pub reason: AwaitingInputReason,
    pub since: String,
}

impl ResolvedModelRecord {
    pub fn new(source: ResolvedModelSource, provider: &str, model_id: &str) -> Self {
        Self {
            provider: provider.to_string(),
            model_id: model_id.to_string(),
            display: format!("{provider}/{model_id}"),
            source,
            variant: None,
            reasoning_effort: None,
            reasoning: None,
            service_tier: None,
        }
    }
}

/// Usage/runtime facts accumulated over one run of a child. `tokens_per_second` is present only
/// when every token-bearing generation window had a non-zero measured duration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskRunStats {
    pub runtime_ms: u64,
    pub turns: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_turns: Option<u64>,
    pub tool_calls: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "js_number")]
    pub tokens_per_second: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "js_number")]
    pub cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "js_number")]
    pub cache_hit_rate_last: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "js_number")]
    pub cache_hit_rate_run: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_status: Option<DurationSourceStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_status: Option<TokenCoverageStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_status: Option<CostReportStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurationSourceStatus {
    Monotonic,
    Reconstructed,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenCoverageStatus {
    Complete,
    Partial,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostReportStatus {
    Reported,
    Partial,
    Invalid,
    Unavailable,
}

/// In-flight stats tagged with the start epoch of the run they describe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveRunStats {
    #[serde(rename = "runStartEpoch")]
    pub run_start_epoch: i64,
    pub stats: TaskRunStats,
}

pub const COST_REPORT_STATUSES: [CostReportStatus; 4] = [
    CostReportStatus::Reported,
    CostReportStatus::Partial,
    CostReportStatus::Unavailable,
    CostReportStatus::Invalid,
];

pub use crate::completion::{
    ParentModelFallback, ParentModelFallbackPhase, ParentModelFallbackReason,
};

pub const PARENT_MODEL_FALLBACK_REASONS: [ParentModelFallbackReason; 5] = [
    ParentModelFallbackReason::UsageLimit,
    ParentModelFallbackReason::NotSignedIn,
    ParentModelFallbackReason::Unavailable,
    ParentModelFallbackReason::Refused,
    ParentModelFallbackReason::Error,
];

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Writes integral floats the way `JSON.stringify` does (`118`, not `118.0`).
pub(crate) fn js_number<S: Serializer>(
    value: &Option<f64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(number) if number.fract() == 0.0 && number.abs() <= MAX_SAFE_INTEGER => {
            serializer.serialize_i64(*number as i64)
        }
        Some(number) => serializer.serialize_f64(*number),
        None => serializer.serialize_none(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskNotification {
    pub run_epoch: i64,
    pub notified_epoch: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notification_failed_epoch: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liveness_notified_epoch: Option<i64>,
}

impl Default for TaskNotification {
    fn default() -> Self {
        Self {
            run_epoch: 0,
            notified_epoch: -1,
            notification_failed_epoch: None,
            liveness_notified_epoch: None,
        }
    }
}

/// A persisted spawn spec. The legacy process-mode shape carries only `cwd` once parsed (its
/// untrusted `extensions`/`member_env` launch inputs are discarded); in-process rebuild requires V1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskSpawnSpec {
    LegacyProcess {
        cwd: String,
        extensions: Option<Vec<String>>,
        member_env: Option<Vec<(String, String)>>,
    },
    V1(SpawnSpecV1),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpecV1 {
    pub cwd: String,
    pub prompt: String,
    pub instructions: Option<String>,
    pub member_scoped_tool_names: Option<Vec<String>>,
    /// The isolation spec of a sandboxed child (TS SpawnSpecV1.isolation).
    pub isolation: Option<TaskIsolationSpec>,
}

impl TaskSpawnSpec {
    pub fn as_v1(&self) -> Option<&SpawnSpecV1> {
        match self {
            Self::V1(spec) => Some(spec),
            Self::LegacyProcess { .. } => None,
        }
    }
}

pub fn is_spawn_spec_v1(spec: &TaskSpawnSpec) -> bool {
    spec.as_v1().is_some()
}

impl Serialize for TaskSpawnSpec {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        match self {
            Self::LegacyProcess {
                cwd,
                extensions,
                member_env,
            } => {
                map.serialize_entry("cwd", cwd)?;
                if let Some(extensions) = extensions {
                    map.serialize_entry("extensions", extensions)?;
                }
                if let Some(member_env) = member_env {
                    let env: serde_json::Map<String, serde_json::Value> = member_env
                        .iter()
                        .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
                        .collect();
                    map.serialize_entry("member_env", &env)?;
                }
            }
            Self::V1(spec) => {
                map.serialize_entry("version", &1)?;
                map.serialize_entry("cwd", &spec.cwd)?;
                map.serialize_entry("prompt", &spec.prompt)?;
                if let Some(instructions) = &spec.instructions {
                    map.serialize_entry("instructions", instructions)?;
                }
                if let Some(names) = &spec.member_scoped_tool_names {
                    map.serialize_entry("member_scoped_tool_names", names)?;
                }
                if let Some(isolation) = &spec.isolation {
                    map.serialize_entry("isolation", isolation)?;
                }
            }
        }
        map.end()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DeliverAs {
    #[serde(rename = "steer")]
    Steer,
    #[serde(rename = "followUp")]
    FollowUp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingSteeringEntry {
    pub id: String,
    pub message: String,
    pub deliver_as: DeliverAs,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workpool: Option<PendingSteeringWorkpoolMetadata>,
}

impl PendingSteeringEntry {
    pub fn new(id: String, message: String, deliver_as: DeliverAs) -> Self {
        Self {
            id,
            message,
            deliver_as,
            workpool: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSteeringWorkpoolMetadata {
    pub pool_id: String,
    pub generation: u32,
    pub item_id: String,
    pub key: String,
    pub run_epoch: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskRecordInput {
    pub name: Option<String>,
    pub task_summary: Option<String>,
    pub description: Option<String>,
    pub parent_session_id: String,
    pub root_session_id: String,
    pub depth: u32,
    pub agent_type: Option<String>,
    pub category: Option<String>,
    pub execution_mode: String,
    pub model: String,
    pub requested_model: Option<ResolvedModelRecord>,
    pub fallback_models: Option<Vec<ResolvedModelRecord>>,
    pub fallback_attempts: Option<Vec<ResolvedModelRecord>>,
    pub resolved_model: Option<ResolvedModelRecord>,
    pub parent_model: Option<String>,
    pub tool_allow: Option<Vec<String>>,
    pub tool_deny: Option<Vec<String>>,
    pub notify_on_terminal: bool,
    pub pending_steering: Option<Vec<PendingSteeringEntry>>,
    pub owner: Option<DagTaskOwner>,
    /// The runner kind of the child (TS TaskRecordInput.runner_kind).
    pub runner_kind: Option<RunnerKind>,
    /// The daemon identity of a host-session child (TS TaskRecordInput.host_session).
    pub host_session: Option<HostSessionIdentity>,
    pub team_run_id: Option<String>,
    pub team_name: Option<String>,
    pub team_member_name: Option<String>,
    pub team_role: Option<String>,
    pub task_seq: Option<u64>,
    pub config_generation: Option<u64>,
    pub background_mode: Option<BackgroundMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundMode {
    Foreground,
    Background,
    Promoted,
}

/// The durable task record; field order mirrors what the TypeScript store writes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub status: TaskStatus,
    pub residency_state: ResidencyState,
    pub parent_session_id: String,
    pub root_session_id: String,
    pub depth: u32,
    pub execution_mode: String,
    pub model: String,
    pub notify_on_terminal: bool,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_at: Option<String>,
    pub notification: TaskNotification,
    #[serde(skip_serializing_if = "super::run_fence::is_legacy_run_start_epoch")]
    pub run_start_epoch: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_member_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_mode: Option<BackgroundMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_model: Option<EffectiveModelRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suspension_reason: Option<SuspensionReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub awaiting_input: Option<TaskAwaitingInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_allow: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_deny: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<ResolvedModelRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_models: Option<Vec<ResolvedModelRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_attempts: Option<Vec<ResolvedModelRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<ResolvedModelRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spawn_spec: Option<TaskSpawnSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<DagTaskOwner>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_steering: Option<Vec<PendingSteeringEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_pid: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub killed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_stats: Option<TaskRunStats>,
    /// The isolation record of an isolated child (TS TaskRecord.isolation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isolation: Option<IsolationRecord>,
    /// The runner kind of the child (TS TaskRecord.runner_kind).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_kind: Option<RunnerKind>,
    /// The daemon identity of a host-session child (TS TaskRecord.host_session).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_session: Option<HostSessionIdentity>,
    /// task_cancel accepted while the child was unreachable (omo#9403). The cancel is final: every
    /// revival reads it and finishes the cancel instead of running the child again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_requested: Option<CancelRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_model_fallback: Option<ParentModelFallback>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_closing_child: Option<FallbackClosingChild>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_queued: Option<StartQueued>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revive_delivery_uncertain: Option<ReviveDeliveryUncertainty>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "js_number")]
    pub prior_runs_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_admission: Option<crate::manager::concurrency::QueuedTaskAdmission>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_pause: Option<RecordedRunPause>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resumed_run_epoch: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub burnt_epoch: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_handoff_epoch: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revival_deferred_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_deadline_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub residency_claim: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FallbackClosingChild {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_session: Option<HostSessionIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_confirmation: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartQueued {
    pub model: String,
    pub queued_at: String,
    pub queue_position: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviveDeliveryUncertainty {
    pub run_epoch: f64,
    pub message_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPauseKind {
    Dag,
    Task,
    Workpool,
    Team,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunPauseRef {
    pub kind: RunPauseKind,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedRunPause {
    pub parent_session_id: String,
    pub parent_pause_seq: u64,
    pub run_ref: RunPauseRef,
}

/// A task_cancel accepted while the child was unreachable (TS `CancelRequest`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelRequest {
    pub requested_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskTransition {
    Pause {
        timestamp: String,
    },
    Unpause {
        timestamp: String,
    },
    Start {
        timestamp: String,
        pid: Option<i64>,
        child_session_id: Option<String>,
    },
    Complete {
        timestamp: String,
        final_response: String,
        run_stats: Option<TaskRunStats>,
    },
    Fail {
        timestamp: String,
        error_message: String,
        killed: bool,
        run_stats: Option<TaskRunStats>,
    },
    Cancel {
        timestamp: String,
        error_message: Option<String>,
        run_stats: Option<TaskRunStats>,
    },
    Interrupt {
        timestamp: String,
        error_message: Option<String>,
        run_stats: Option<TaskRunStats>,
    },
    Lose {
        timestamp: String,
        error_message: String,
    },
    Evict {
        timestamp: String,
    },
    Dispose {
        timestamp: String,
    },
    PersistOnly {
        timestamp: String,
    },
    DetachRpc {
        timestamp: String,
    },
    MarkResident {
        timestamp: String,
    },
}

impl TaskTransition {
    pub fn timestamp(&self) -> &str {
        match self {
            Self::Pause { timestamp }
            | Self::Unpause { timestamp }
            | Self::Start { timestamp, .. }
            | Self::Complete { timestamp, .. }
            | Self::Fail { timestamp, .. }
            | Self::Cancel { timestamp, .. }
            | Self::Interrupt { timestamp, .. }
            | Self::Lose { timestamp, .. }
            | Self::Evict { timestamp }
            | Self::Dispose { timestamp }
            | Self::PersistOnly { timestamp }
            | Self::DetachRpc { timestamp }
            | Self::MarkResident { timestamp } => timestamp,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskTransitionAudit {
    TransitionApplied {
        status: TaskStatus,
        residency_state: ResidencyState,
    },
    LateTransitionIgnored {
        attempted_status: TaskStatus,
        current_status: TaskStatus,
    },
    InvalidTransitionIgnored {
        attempted_status: TaskStatus,
        current_status: TaskStatus,
    },
}

impl TaskTransitionAudit {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::TransitionApplied { .. } => "transition_applied",
            Self::LateTransitionIgnored { .. } => "late_transition_ignored",
            Self::InvalidTransitionIgnored { .. } => "invalid_transition_ignored",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskTransitionResult {
    pub applied: bool,
    pub record: TaskRecord,
    pub audit: TaskTransitionAudit,
}

/// The identity of a child that runs as a SESSION of the shared rpc-host daemon rather than a
/// process (upstream `state/types.ts` HostSessionIdentity). A host session has no pid of its own:
/// its liveness is the daemon answering plus the daemon still listing its session path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSessionIdentity {
    pub socket: String,
    pub routing_id: String,
    pub session_path: String,
    pub instance_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_pid: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adopted_entry_id: Option<String>,
}

/// The runner kind of a task: a child process, or a session of the shared daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerKind {
    ChildProcess,
    HostSession,
}
