//! `dag/types.ts`: the public type contract of the DAG subsystem.
// allow: SIZE_OK - single public type contract for the dag subsystem, mirroring types.ts.

use serde::{Deserialize, Serialize};

use crate::state::TaskRunStats;

pub type DagRunId = String;
pub type DagNodeId = String;

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
    Completed,
    Failed,
    Cancelled,
    Skipped,
}

pub const DAG_NODE_STATES: [DagNodeState; 8] = [
    DagNodeState::Pending,
    DagNodeState::Blocked,
    DagNodeState::Scheduled,
    DagNodeState::Running,
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
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
        }
    }

    pub fn is_terminal(self) -> bool {
        match self {
            Self::Completed | Self::Failed | Self::Cancelled | Self::Skipped => true,
            Self::Pending | Self::Blocked | Self::Scheduled | Self::Running => false,
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
    JournalCorrupt,
}

pub const DAG_NODE_ERROR_CODES: [DagNodeErrorCode; 10] = [
    DagNodeErrorCode::PlanUnresolved,
    DagNodeErrorCode::DepthDenied,
    DagNodeErrorCode::StartFailed,
    DagNodeErrorCode::ResidencyDenied,
    DagNodeErrorCode::TaskError,
    DagNodeErrorCode::TaskInterrupted,
    DagNodeErrorCode::TaskLost,
    DagNodeErrorCode::TaskCancelled,
    DagNodeErrorCode::ResumeTaskMissing,
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
}

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagNodeCounts {
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
    TaskQueued {
        #[serde(rename = "queuePosition")]
        queue_position: u64,
    },
}

pub const DAG_NODE_TRANSITION_REASONS: [DagNodeTransitionReason; 10] = [
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
];

pub const DAG_RUN_EVENT_TYPES: [&str; 14] = [
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
    "dag.diagnostic.added",
    "dag.stream.overflow",
];

/// The journaled payload union: exactly 14 members, every one written with a WAL seq.
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
    #[serde(rename = "dag.diagnostic.added")]
    DiagnosticAdded { diagnostic: DagDiagnostic },
    #[serde(rename = "dag.stream.overflow", rename_all = "camelCase")]
    StreamOverflow {
        dropped_count: u64,
        recover_after_seq: u64,
    },
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
    DiagnosticAdded,
    StreamOverflow,
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
            Self::DiagnosticAdded => "dag.diagnostic.added",
            Self::StreamOverflow => "dag.stream.overflow",
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
            Self::DiagnosticAdded { .. } => DagRunEventType::DiagnosticAdded,
            Self::StreamOverflow { .. } => DagRunEventType::StreamOverflow,
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
