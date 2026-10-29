//! `dag/types.ts`: the public type contract of the DAG subsystem.
// allow: SIZE_OK - single public type contract for the dag subsystem, mirroring types.ts.

use serde::{Deserialize, Serialize};

use crate::state::TaskRunStats;

pub type DagRunId = String;
pub type DagNodeId = String;

/// A run or node id used as a single filesystem path segment.
pub fn is_safe_dag_path_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains('\0')
        && !value.contains(':')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DagRunStatus {
    Pending,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

pub const DAG_RUN_STATUSES: [DagRunStatus; 6] = [
    DagRunStatus::Pending,
    DagRunStatus::Running,
    DagRunStatus::Paused,
    DagRunStatus::Completed,
    DagRunStatus::Failed,
    DagRunStatus::Cancelled,
];

impl DagRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        match self {
            Self::Completed | Self::Failed | Self::Cancelled => true,
            Self::Pending | Self::Running | Self::Paused => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DagNodeState {
    Pending,
    Blocked,
    Scheduled,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
    Skipped,
}

pub const DAG_NODE_STATES: [DagNodeState; 9] = [
    DagNodeState::Pending,
    DagNodeState::Blocked,
    DagNodeState::Scheduled,
    DagNodeState::Running,
    DagNodeState::Paused,
    DagNodeState::Completed,
    DagNodeState::Failed,
    DagNodeState::Cancelled,
    DagNodeState::Skipped,
];

impl DagNodeState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Blocked => "blocked",
            Self::Scheduled => "scheduled",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
        }
    }

    pub fn is_terminal(self) -> bool {
        match self {
            Self::Completed | Self::Failed | Self::Cancelled | Self::Skipped => true,
            Self::Pending | Self::Blocked | Self::Scheduled | Self::Running | Self::Paused => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DagEventLane {
    Activity,
    Boundary,
}

pub const DAG_EVENT_LANES: [&str; 2] = ["activity", "boundary"];

/// Route contract: category XOR agent. A pure-model route is unrepresentable.
pub const DAG_ROUTE_KINDS: [&str; 2] = ["category", "agent"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DagRoute {
    Category {
        category: String,
    },
    Agent {
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
}

/// Node-level user input: category XOR subagent_type, model only alongside subagent_type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DagNodeTarget {
    Category(String),
    SubagentType {
        subagent_type: String,
        model: Option<String>,
    },
}

/// Mirrors `TaskTargetErrorCode` from the task tool validation.
pub type DagNodeTargetErrorCode = crate::tools::task::validation::TaskTargetErrorCode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagNodeTargetError {
    pub code: DagNodeTargetErrorCode,
    pub message: String,
}

/// Optional `task.dag` settings block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DagSettings {
    pub max_nodes_per_run: usize,
    pub max_runs_per_session: usize,
    pub subscriber_ring: usize,
    pub heartbeat_ms: u64,
    pub history_default_limit: usize,
    pub history_max_limit: usize,
    pub retention_days: u64,
    pub max_prompt_bytes: usize,
}

pub const DAG_SETTINGS_DEFAULTS: DagSettings = DagSettings {
    max_nodes_per_run: 64,
    max_runs_per_session: 16,
    subscriber_ring: 1000,
    heartbeat_ms: 15000,
    history_default_limit: 256,
    history_max_limit: 1000,
    retention_days: 7,
    max_prompt_bytes: 262_144,
};

impl Default for DagSettings {
    fn default() -> Self {
        DAG_SETTINGS_DEFAULTS
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DagNodeErrorCode {
    PlanUnresolved,
    DepthDenied,
    StartFailed,
    ResidencyDenied,
    TaskError,
    TaskInterrupted,
    TaskLost,
    TaskCancelled,
    ResumeTaskMissing,
    ResumeTaskOrphaned,
    LostWhilePaused,
    JournalCorrupt,
}

pub const DAG_NODE_ERROR_CODES: [DagNodeErrorCode; 12] = [
    DagNodeErrorCode::PlanUnresolved,
    DagNodeErrorCode::DepthDenied,
    DagNodeErrorCode::StartFailed,
    DagNodeErrorCode::ResidencyDenied,
    DagNodeErrorCode::TaskError,
    DagNodeErrorCode::TaskInterrupted,
    DagNodeErrorCode::TaskLost,
    DagNodeErrorCode::TaskCancelled,
    DagNodeErrorCode::ResumeTaskMissing,
    DagNodeErrorCode::ResumeTaskOrphaned,
    DagNodeErrorCode::LostWhilePaused,
    DagNodeErrorCode::JournalCorrupt,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagNodeError {
    pub code: DagNodeErrorCode,
    pub message: String,
    #[serde(rename = "nodeId", default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<DagNodeId>,
    pub at: String,
}

/// Marks a `dag.node.retried` event as an automatic infrastructure retry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagNodeRetryAutomatic {
    pub attempt: u32,
    pub failure_kind: String,
    pub delay_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DagDiagnostic {
    RouteFallback {
        #[serde(rename = "nodeId")]
        node_id: DagNodeId,
        message: String,
        at: String,
    },
    NodeFlag {
        #[serde(rename = "nodeId")]
        node_id: DagNodeId,
        message: String,
        at: String,
    },
    MissingSkill {
        #[serde(rename = "nodeId")]
        node_id: DagNodeId,
        skill: String,
        message: String,
        at: String,
    },
    RunFlag {
        message: String,
        at: String,
    },
    JournalCorrupt {
        #[serde(rename = "runId", default, skip_serializing_if = "Option::is_none")]
        run_id: Option<DagRunId>,
        path: String,
        message: String,
        at: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagNode {
    pub id: DagNodeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub prompt: String,
    pub route: DagRoute,
    pub depends_on: Vec<DagNodeId>,
    pub state: DagNodeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<DagNodeError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_stats: Option<TaskRunStats>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
}

pub const DAG_NODE_OUTPUT_PREVIEW_CHARS: usize = 2000;
pub const DAG_NODE_QUIET_AFTER_MS: u64 = 600_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagEdge {
    pub from: DagNodeId,
    pub to: DagNodeId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagWave {
    pub index: usize,
    pub node_ids: Vec<DagNodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagBottleneck {
    pub node_id: DagNodeId,
    pub blocked_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagRunPause {
    pub reason: String,
    pub pause_seq: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagNodeCounts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<usize>,
    pub total: usize,
    pub pending: usize,
    pub blocked: usize,
    pub scheduled: usize,
    pub running: usize,
    pub completed: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub skipped: usize,
}

/// The `schemaVersion: 1` literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct SchemaVersion1;

impl From<SchemaVersion1> for u8 {
    fn from(_: SchemaVersion1) -> Self {
        1
    }
}

impl TryFrom<u8> for SchemaVersion1 {
    type Error = String;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value == 1 {
            Ok(Self)
        } else {
            Err(format!("unsupported schemaVersion {value}"))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagRunSnapshot {
    pub schema_version: SchemaVersion1,
    pub run_id: DagRunId,
    pub run_key: String,
    pub name: String,
    pub parent_session_id: String,
    pub root_session_id: String,
    pub status: DagRunStatus,
    pub generation: u64,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    pub definition_fingerprint: String,
    pub last_seq: u64,
    pub nodes: Vec<DagNode>,
    pub edges: Vec<DagEdge>,
    pub waves: Vec<DagWave>,
    pub critical_path: Vec<DagNodeId>,
    pub bottlenecks: Vec<DagBottleneck>,
    pub diagnostics: Vec<DagDiagnostic>,
    pub counts: DagNodeCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause: Option<DagRunPause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amend_history: Option<Vec<AmendRecord>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_holder_pid: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AmendRecord {
    pub at: String,
    pub previous_fingerprint: String,
    pub fingerprint: String,
    pub changed_node_ids: Vec<DagNodeId>,
    pub added_node_ids: Vec<DagNodeId>,
    pub invalidated_node_ids: Vec<DagNodeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DagNodeTransitionReason {
    Unblocked,
    Scheduled,
    Started,
    Succeeded,
    Failed,
    Cancelled,
    Skipped,
    Interrupted,
    Lost,
    Resumed,
    Retried,
    AmendInvalidated,
    Revived,
    TaskQueued {
        #[serde(rename = "queuePosition")]
        queue_position: u64,
    },
    ResidencyQueued {
        residents: u64,
        #[serde(rename = "heldByOtherOwners")]
        held_by_other_owners: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DagNodeDelivery {
    Steer,
    Revive,
}

pub const DAG_NODE_TRANSITION_REASONS: [DagNodeTransitionReason; 13] = [
    DagNodeTransitionReason::Unblocked,
    DagNodeTransitionReason::Scheduled,
    DagNodeTransitionReason::Started,
    DagNodeTransitionReason::Succeeded,
    DagNodeTransitionReason::Failed,
    DagNodeTransitionReason::Cancelled,
    DagNodeTransitionReason::Skipped,
    DagNodeTransitionReason::Interrupted,
    DagNodeTransitionReason::Lost,
    DagNodeTransitionReason::Resumed,
    DagNodeTransitionReason::Retried,
    DagNodeTransitionReason::AmendInvalidated,
    DagNodeTransitionReason::Revived,
];

pub const DAG_RUN_EVENT_TYPES: [&str; 26] = [
    "dag.run.created",
    "dag.run.started",
    "dag.run.paused",
    "dag.run.resumed",
    "dag.run.completed",
    "dag.run.failed",
    "dag.run.cancelled",
    "dag.wave.started",
    "dag.wave.completed",
    "dag.node.transitioned",
    "dag.node.task-attached",
    "dag.node.reused",
    "dag.node.retried",
    "dag.node.steered",
    "dag.definition.amended",
    "dag.diagnostic.added",
    "dag.stream.overflow",
    "dag.guidance.added",
    "dag.guidance.edited",
    "dag.guidance.removed",
    "dag.guidance.dispatching",
    "dag.guidance.delivered",
    "dag.node.cancel_requested",
    "dag.node.retry-staged",
    "dag.node.pause-recovered",
    "dag.node.pause-lost",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidanceState {
    Draft,
    Dispatching,
    Delivered,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GuidanceScope {
    Run,
    Node {
        #[serde(rename = "nodeId")]
        node_id: DagNodeId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuidanceDelivery {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Guidance {
    pub id: String,
    pub revision: u64,
    pub scope: GuidanceScope,
    pub text: String,
    pub state: GuidanceState,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<String>,
    #[serde(default)]
    pub deliveries: Vec<GuidanceDelivery>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuidanceErrorCode {
    NotFound,
    Frozen,
    DuplicateId,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("guidance error: {code:?}")]
pub struct GuidanceError {
    pub code: GuidanceErrorCode,
}

pub use crate::pause::admission_barrier::ResumeCredential;

/// The journaled payload union; every member is written to the WAL with a WAL seq.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DagRunEventPayload {
    #[serde(rename = "dag.run.created", rename_all = "camelCase")]
    RunCreated {
        run_key: String,
        name: String,
        definition_fingerprint: String,
        node_count: usize,
        edge_count: usize,
    },
    #[serde(rename = "dag.run.started")]
    RunStarted { generation: u64 },
    #[serde(rename = "dag.run.paused")]
    RunPaused {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(rename = "pauseSeq", default, skip_serializing_if = "Option::is_none")]
        pause_seq: Option<u64>,
    },
    #[serde(rename = "dag.run.resumed")]
    RunResumed { generation: u64 },
    #[serde(rename = "dag.run.completed")]
    RunCompleted { counts: DagNodeCounts },
    #[serde(rename = "dag.run.failed")]
    RunFailed {
        error: DagNodeError,
        counts: DagNodeCounts,
    },
    #[serde(rename = "dag.run.cancelled")]
    RunCancelled {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        counts: DagNodeCounts,
    },
    #[serde(rename = "dag.wave.started", rename_all = "camelCase")]
    WaveStarted {
        wave_index: usize,
        node_ids: Vec<DagNodeId>,
    },
    #[serde(rename = "dag.wave.completed", rename_all = "camelCase")]
    WaveCompleted {
        wave_index: usize,
        node_ids: Vec<DagNodeId>,
    },
    #[serde(rename = "dag.node.transitioned", rename_all = "camelCase")]
    NodeTransitioned {
        node_id: DagNodeId,
        from: DagNodeState,
        to: DagNodeState,
        reason: DagNodeTransitionReason,
    },
    #[serde(rename = "dag.node.task-attached", rename_all = "camelCase")]
    NodeTaskAttached {
        node_id: DagNodeId,
        task_id: String,
        attempt: u32,
    },
    #[serde(rename = "dag.node.reused", rename_all = "camelCase")]
    NodeReused {
        node_id: DagNodeId,
        task_id: String,
        source_run_id: DagRunId,
    },
    #[serde(rename = "dag.node.retried", rename_all = "camelCase")]
    NodeRetried {
        node_id: DagNodeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prior_task_id: Option<String>,
        exec_attempt: u32,
        prompt_changed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        automatic: Option<DagNodeRetryAutomatic>,
    },
    #[serde(rename = "dag.node.steered", rename_all = "camelCase")]
    NodeSteered {
        node_id: DagNodeId,
        task_id: String,
        delivery: DagNodeDelivery,
    },
    #[serde(rename = "dag.definition.amended", rename_all = "camelCase")]
    DefinitionAmended {
        previous_fingerprint: String,
        fingerprint: String,
        changed_node_ids: Vec<DagNodeId>,
        added_node_ids: Vec<DagNodeId>,
        invalidated_node_ids: Vec<DagNodeId>,
        definition: crate::dag::manager::DagPersistedDefinition,
        nodes: Vec<DagNode>,
        edges: Vec<DagEdge>,
        waves: Vec<DagWave>,
        critical_path: Vec<DagNodeId>,
        bottlenecks: Vec<DagBottleneck>,
        diagnostics: Vec<DagDiagnostic>,
    },
    #[serde(rename = "dag.diagnostic.added")]
    DiagnosticAdded { diagnostic: DagDiagnostic },
    #[serde(rename = "dag.stream.overflow", rename_all = "camelCase")]
    StreamOverflow {
        dropped_count: u64,
        recover_after_seq: u64,
    },
    #[serde(rename = "dag.guidance.added")]
    GuidanceAdded { guidance: Guidance },
    #[serde(rename = "dag.guidance.edited")]
    GuidanceEdited { guidance: Guidance },
    #[serde(rename = "dag.guidance.removed")]
    GuidanceRemoved { guidance: Guidance },
    #[serde(rename = "dag.guidance.dispatching")]
    GuidanceDispatching { guidance: Guidance },
    #[serde(rename = "dag.guidance.delivered")]
    GuidanceDelivered { guidance: Guidance },
    #[serde(rename = "dag.node.cancel_requested", rename_all = "camelCase")]
    NodeCancelRequested { node_id: DagNodeId },
    #[serde(rename = "dag.node.retry-staged", rename_all = "camelCase")]
    NodeRetryStaged { node_ids: Vec<DagNodeId> },
    #[serde(rename = "dag.node.pause-recovered", rename_all = "camelCase")]
    NodePauseRecovered { node_id: DagNodeId, child_seq: u64 },
    #[serde(rename = "dag.node.pause-lost", rename_all = "camelCase")]
    NodePauseLost { node_id: DagNodeId, error: DagNodeError },
}

/// The discriminator of a [`DagRunEventPayload`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DagRunEventType {
    RunCreated,
    RunStarted,
    RunPaused,
    RunResumed,
    RunCompleted,
    RunFailed,
    RunCancelled,
    WaveStarted,
    WaveCompleted,
    NodeTransitioned,
    NodeTaskAttached,
    NodeReused,
    NodeRetried,
    NodeSteered,
    DefinitionAmended,
    DiagnosticAdded,
    StreamOverflow,
    GuidanceAdded,
    GuidanceEdited,
    GuidanceRemoved,
    GuidanceDispatching,
    GuidanceDelivered,
    NodeCancelRequested,
    NodeRetryStaged,
    NodePauseRecovered,
    NodePauseLost,
}

impl DagRunEventType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunCreated => "dag.run.created",
            Self::RunStarted => "dag.run.started",
            Self::RunPaused => "dag.run.paused",
            Self::RunResumed => "dag.run.resumed",
            Self::RunCompleted => "dag.run.completed",
            Self::RunFailed => "dag.run.failed",
            Self::RunCancelled => "dag.run.cancelled",
            Self::WaveStarted => "dag.wave.started",
            Self::WaveCompleted => "dag.wave.completed",
            Self::NodeTransitioned => "dag.node.transitioned",
            Self::NodeTaskAttached => "dag.node.task-attached",
            Self::NodeReused => "dag.node.reused",
            Self::NodeRetried => "dag.node.retried",
            Self::NodeSteered => "dag.node.steered",
            Self::DefinitionAmended => "dag.definition.amended",
            Self::DiagnosticAdded => "dag.diagnostic.added",
            Self::StreamOverflow => "dag.stream.overflow",
            Self::GuidanceAdded => "dag.guidance.added",
            Self::GuidanceEdited => "dag.guidance.edited",
            Self::GuidanceRemoved => "dag.guidance.removed",
            Self::GuidanceDispatching => "dag.guidance.dispatching",
            Self::GuidanceDelivered => "dag.guidance.delivered",
            Self::NodeCancelRequested => "dag.node.cancel_requested",
            Self::NodeRetryStaged => "dag.node.retry-staged",
            Self::NodePauseRecovered => "dag.node.pause-recovered",
            Self::NodePauseLost => "dag.node.pause-lost",
        }
    }
}

impl DagRunEventPayload {
    pub fn event_type(&self) -> DagRunEventType {
        match self {
            Self::RunCreated { .. } => DagRunEventType::RunCreated,
            Self::RunStarted { .. } => DagRunEventType::RunStarted,
            Self::RunPaused { .. } => DagRunEventType::RunPaused,
            Self::RunResumed { .. } => DagRunEventType::RunResumed,
            Self::RunCompleted { .. } => DagRunEventType::RunCompleted,
            Self::RunFailed { .. } => DagRunEventType::RunFailed,
            Self::RunCancelled { .. } => DagRunEventType::RunCancelled,
            Self::WaveStarted { .. } => DagRunEventType::WaveStarted,
            Self::WaveCompleted { .. } => DagRunEventType::WaveCompleted,
            Self::NodeTransitioned { .. } => DagRunEventType::NodeTransitioned,
            Self::NodeTaskAttached { .. } => DagRunEventType::NodeTaskAttached,
            Self::NodeReused { .. } => DagRunEventType::NodeReused,
            Self::NodeRetried { .. } => DagRunEventType::NodeRetried,
            Self::NodeSteered { .. } => DagRunEventType::NodeSteered,
            Self::DefinitionAmended { .. } => DagRunEventType::DefinitionAmended,
            Self::DiagnosticAdded { .. } => DagRunEventType::DiagnosticAdded,
            Self::StreamOverflow { .. } => DagRunEventType::StreamOverflow,
            Self::GuidanceAdded { .. } => DagRunEventType::GuidanceAdded,
            Self::GuidanceEdited { .. } => DagRunEventType::GuidanceEdited,
            Self::GuidanceRemoved { .. } => DagRunEventType::GuidanceRemoved,
            Self::GuidanceDispatching { .. } => DagRunEventType::GuidanceDispatching,
            Self::GuidanceDelivered { .. } => DagRunEventType::GuidanceDelivered,
            Self::NodeCancelRequested { .. } => DagRunEventType::NodeCancelRequested,
            Self::NodeRetryStaged { .. } => DagRunEventType::NodeRetryStaged,
            Self::NodePauseRecovered { .. } => DagRunEventType::NodePauseRecovered,
            Self::NodePauseLost { .. } => DagRunEventType::NodePauseLost,
        }
    }
}

/// The flat wire event: envelope fields and payload fields are siblings on one object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagRunEvent {
    pub schema_version: SchemaVersion1,
    pub run_id: DagRunId,
    pub seq: u64,
    pub at: String,
    pub lane: DagEventLane,
    #[serde(flatten)]
    pub payload: DagRunEventPayload,
}

pub const DAG_ACTIVITY_CHANNEL: &str = "omo.dag.activity";

/// Live activity telemetry: unsequenced, never journaled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagActivityEvent {
    pub schema_version: SchemaVersion1,
    pub run_id: DagRunId,
    pub node_id: DagNodeId,
    pub task_id: String,
    pub at: String,
    pub activity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assistant_line: Option<String>,
    pub turns: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<u64>,
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod tests;
