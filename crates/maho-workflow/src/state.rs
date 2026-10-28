//! Run and node states (spec §2) and the persisted run record.
use crate::definition::WorkflowDefinition;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Length of the stored output preview, in characters.
pub const PREVIEW_CHARS: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Pending,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl RunStatus {
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
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
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

impl NodeState {
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
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled | Self::Skipped)
    }
    /// Has a child that may still be working.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Scheduled | Self::Running)
    }
}

/// Node error codes (spec §2, interface facts).
pub mod codes {
    pub const PLAN_UNRESOLVED: &str = "plan_unresolved";
    pub const DEPTH_DENIED: &str = "depth_denied";
    pub const START_FAILED: &str = "start_failed";
    pub const RESIDENCY_DENIED: &str = "residency_denied";
    pub const TASK_ERROR: &str = "task_error";
    pub const TASK_INTERRUPTED: &str = "task_interrupted";
    pub const TASK_LOST: &str = "task_lost";
    pub const TASK_CANCELLED: &str = "task_cancelled";
    pub const RESUME_TASK_MISSING: &str = "resume_task_missing";
    pub const RESUME_TASK_ORPHANED: &str = "resume_task_orphaned";
    pub const LOST_WHILE_PAUSED: &str = "lost_while_paused";
    pub const JOURNAL_CORRUPT: &str = "journal_corrupt";
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeError {
    pub code: String,
    pub message: String,
    pub at: u64,
}

impl NodeError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            at: crate::now_ms(),
        }
    }
}

/// Runtime state of one node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRecord {
    pub id: String,
    pub state: NodeState,
    pub attempt: u32,
    /// The prompt dispatched to the child (skills prepended at creation, §3.2).
    pub prompt: String,
    /// The skill text prepended to `prompt` (kept so a retry with a new prompt keeps it).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub skill_prefix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<NodeError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<usize>,
    /// Automatic infrastructure retries used in the current attempt (survives restarts).
    #[serde(default)]
    pub infra_retries: u32,
    /// Time of the child's last observed activity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<u64>,
    /// Guidance notes to append to this node's next dispatch (delivered on user resume).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_guidance: Vec<String>,
}

impl NodeRecord {
    pub fn new(id: &str, prompt: String) -> Self {
        Self {
            id: id.to_owned(),
            state: NodeState::Pending,
            attempt: 1,
            prompt,
            skill_prefix: String::new(),
            task_id: None,
            started_at: None,
            finished_at: None,
            error: None,
            output_preview: None,
            output_bytes: None,
            infra_retries: 0,
            last_activity_at: None,
            pending_guidance: Vec::new(),
        }
    }

    /// Back to `pending` for a fresh attempt (retry/amend).
    pub fn reset(&mut self, next_attempt: bool) {
        self.state = NodeState::Pending;
        if next_attempt {
            self.attempt += 1;
        }
        self.task_id = None;
        self.started_at = None;
        self.finished_at = None;
        self.error = None;
        self.output_preview = None;
        self.output_bytes = None;
        self.infra_retries = 0;
        self.last_activity_at = None;
    }

    /// Duration in ms when started (and finished, or until `now`).
    pub fn duration_ms(&self, now: u64) -> Option<u64> {
        let start = self.started_at?;
        Some(self.finished_at.unwrap_or(now).saturating_sub(start))
    }
}

/// A queued guidance note (delivered on user resume).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Guidance {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// Delivery has begun; the note can no longer change.
    #[serde(default)]
    pub frozen: bool,
}

/// One run, as persisted in `runs/<run id>.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub version: u32,
    pub run_id: String,
    pub key: String,
    pub name: String,
    pub parent_session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_session: Option<String>,
    pub status: RunStatus,
    pub definition: WorkflowDefinition,
    pub fingerprint: String,
    /// Node states by id.
    pub nodes: BTreeMap<String, NodeRecord>,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<NodeError>,
    #[serde(default)]
    pub amendments: u32,
    /// Stopped by the user (only the user-facing resume control restarts it).
    #[serde(default)]
    pub user_paused: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guidance: Vec<Guidance>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Pid of the owning process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pid: Option<u32>,
    /// Last lease refresh of the owning process.
    #[serde(default)]
    pub lease_at: u64,
}

pub const RECORD_VERSION: u32 = 1;

/// Characters `[..PREVIEW_CHARS]` of `text`.
pub fn preview(text: &str) -> String {
    text.chars().take(PREVIEW_CHARS).collect()
}

impl WorkflowRun {
    pub fn node(&self, id: &str) -> Option<&NodeRecord> {
        self.nodes.get(id)
    }

    /// `(completed, total, running, failed)`.
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut completed = 0;
        let mut running = 0;
        let mut failed = 0;
        for node in self.nodes.values() {
            match node.state {
                NodeState::Completed => completed += 1,
                NodeState::Running | NodeState::Scheduled => running += 1,
                NodeState::Failed => failed += 1,
                _ => {}
            }
        }
        (completed, self.nodes.len(), running, failed)
    }

    pub fn state_counts(&self) -> BTreeMap<&'static str, usize> {
        let mut counts = BTreeMap::new();
        for node in self.nodes.values() {
            *counts.entry(node.state.as_str()).or_default() += 1;
        }
        counts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_is_char_bounded() {
        let text = "\u{e9}".repeat(PREVIEW_CHARS + 5);
        assert_eq!(preview(&text).chars().count(), PREVIEW_CHARS);
    }

    #[test]
    fn reset_bumps_attempt() {
        let mut node = NodeRecord::new("a", "p".into());
        node.state = NodeState::Failed;
        node.error = Some(NodeError::new(codes::TASK_ERROR, "x"));
        node.reset(true);
        assert_eq!(node.attempt, 2);
        assert_eq!(node.state, NodeState::Pending);
        assert!(node.error.is_none());
    }
}
