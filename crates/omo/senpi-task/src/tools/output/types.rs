//! Port of `tools/output/types.ts`.

use std::io;
use std::sync::Arc;

use serde::Serialize;
use serde::ser::Serializer;

use crate::manager::manager::TaskManager;
use crate::manager::types::{ListScope, ListedTask};
use crate::state::{ResidencyState, ResolvedModelRecord, TaskRecord, TaskRunStats, TaskStatus};
use crate::tools::control::tool_result::AgentToolResult;
use crate::tools::control::types::{CallerSessionResolver, serialize_task_status};

/// `Pick<TaskManager, "get" | "list">`.
pub trait OutputManager: Send + Sync {
    fn get(&self, task_id: &str) -> Option<TaskRecord>;
    fn list(&self, scope: &ListScope) -> Vec<ListedTask>;
}

impl OutputManager for TaskManager {
    fn get(&self, task_id: &str) -> Option<TaskRecord> {
        TaskManager::get(self, task_id)
    }

    fn list(&self, scope: &ListScope) -> Vec<ListedTask> {
        TaskManager::list(self, scope)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TranscriptEntry {
    Assistant { text: String },
    Tool { tool: String, is_error: bool },
    Error { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TranscriptSource {
    #[serde(rename = "event-log")]
    EventLog,
    #[serde(rename = "session-jsonl")]
    SessionJsonl,
    #[serde(rename = "none")]
    None,
}

impl TranscriptSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EventLog => "event-log",
            Self::SessionJsonl => "session-jsonl",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptReadResult {
    pub entries: Vec<TranscriptEntry>,
    pub source: TranscriptSource,
    pub truncated: Option<bool>,
}

/// Input of a `TranscriptReader` (`{ taskId, stateDir }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscriptReaderInput<'a> {
    pub task_id: &'a str,
    pub state_dir: &'a str,
}

/// A transcript reader; I/O failures other than absent state propagate (the TS throws).
pub type TranscriptReader =
    Arc<dyn Fn(&TranscriptReaderInput<'_>) -> io::Result<TranscriptReadResult> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LostBreadcrumbs {
    pub explanation: String,
    pub session_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SuspendedDetails {
    pub explanation: String,
}

pub fn serialize_residency_state<S: Serializer>(state: &ResidencyState, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(state.as_str())
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskSnapshot {
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_summary: Option<String>,
    #[serde(serialize_with = "serialize_task_status")]
    pub status: TaskStatus,
    #[serde(serialize_with = "serialize_residency_state")]
    pub residency_state: ResidencyState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suspended: Option<SuspendedDetails>,
    pub execution_mode: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<ResolvedModelRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub parent_session_id: String,
    pub root_session_id: String,
    pub age_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_stats: Option<TaskRunStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost: Option<LostBreadcrumbs>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptMode {
    Tail,
    Full,
}

impl TranscriptMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tail => "tail",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskOutputDetails {
    Status {
        snapshot: TaskSnapshot,
    },
    Transcript {
        mode: TranscriptMode,
        source: TranscriptSource,
        transcript: String,
        truncated: bool,
        snapshot: TaskSnapshot,
    },
    NotFound {
        reason: String,
        known_tasks: Vec<String>,
    },
    InvalidArguments {
        reason: String,
    },
}

impl TaskOutputDetails {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Status { .. } => "status",
            Self::Transcript { .. } => "transcript",
            Self::NotFound { .. } => "not_found",
            Self::InvalidArguments { .. } => "invalid_arguments",
        }
    }
}

pub type OutputClock = Arc<dyn Fn() -> i64 + Send + Sync>;

#[derive(Clone)]
pub struct TaskOutputDeps {
    pub manager: Arc<dyn OutputManager>,
    pub state_dir: String,
    pub transcript_reader: Option<TranscriptReader>,
    pub resolve_caller_session_id: Option<CallerSessionResolver>,
    pub now: Option<OutputClock>,
}

pub type TaskOutputToolResult = AgentToolResult<TaskOutputDetails>;
