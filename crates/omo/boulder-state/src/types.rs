//! Typed values exchanged with callers: enums, plan progress, lookup inputs.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// How a session joined a work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoulderSessionOrigin {
    Direct,
    Appended,
}

impl BoulderSessionOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Appended => "appended",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "direct" => Some(Self::Direct),
            "appended" => Some(Self::Appended),
            _ => None,
        }
    }
}

/// Lifecycle status of a work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoulderWorkStatus {
    Active,
    Completed,
    Paused,
    Abandoned,
}

impl BoulderWorkStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Paused => "paused",
            Self::Abandoned => "abandoned",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "completed" => Some(Self::Completed),
            "paused" => Some(Self::Paused),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }
}

/// Lifecycle status of a task session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoulderTaskStatus {
    Running,
    Completed,
    Cancelled,
}

impl BoulderTaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// Host platform used to prefix bare session ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SessionPlatform {
    Codex,
    #[default]
    Opencode,
    Senpi,
}

impl SessionPlatform {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Opencode => "opencode",
            Self::Senpi => "senpi",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanProgress {
    pub total: usize,
    pub completed: usize,
    pub is_complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanChecklist {
    pub total: usize,
    pub completed: usize,
    pub remaining: usize,
    pub next_task_label: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopLevelTaskSection {
    Todo,
    FinalWave,
}

impl TopLevelTaskSection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::FinalWave => "final-wave",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopLevelTaskRef {
    pub key: String,
    pub section: TopLevelTaskSection,
    pub label: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoulderWorkResumeOption {
    pub work_id: String,
    pub plan_name: String,
    pub active_plan: String,
    pub worktree_path: Option<String>,
    pub status: BoulderWorkStatus,
    pub started_at: String,
    pub updated_at: String,
    pub ended_at: Option<String>,
    pub elapsed_ms: Option<i64>,
    pub session_count: usize,
    pub progress: PlanProgress,
    pub is_current_mirror: bool,
}

/// Optional owner fields of a new work (`agent`, `worktreePath` in the TypeScript API).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkOwner {
    pub agent: Option<String>,
    pub worktree_path: Option<String>,
}

/// Input of [`crate::add_boulder_work`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoulderWorkInput {
    pub plan_path: String,
    pub session_id: String,
    pub owner: WorkOwner,
    pub started_at: Option<String>,
}

/// Input of [`crate::complete_boulder`]; `work_id` defaults to the active work and
/// `ended_at` to now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompleteBoulderInput {
    pub work_id: Option<String>,
    pub ended_at: Option<String>,
}

/// Task session fields written by the upsert and timer entry points.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskSessionInput {
    pub task_key: String,
    pub task_label: String,
    pub task_title: String,
    pub session_id: String,
    pub agent: Option<String>,
    pub category: Option<String>,
}

/// Input of [`crate::start_task_timer`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskTimerInput {
    pub task: TaskSessionInput,
    pub started_at: Option<String>,
}

/// Input of [`crate::end_task_timer`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EndTaskTimerInput {
    pub task_key: String,
    pub ended_at: Option<String>,
}

/// Filter of [`crate::get_work_by_plan_name`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkLookupOptions {
    pub worktree_path: Option<String>,
}

/// Optional knobs of [`crate::reconcile_stale_works`] (`ReconcileStaleWorksOptions`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileStaleWorksOptions {
    /// Clock override in Unix milliseconds; defaults to `Date.now()`.
    pub now: Option<i64>,
    /// Threshold override in milliseconds; defaults to the resolved env value.
    pub threshold_ms: Option<i64>,
    /// Agent sessions root holding `<encoded session cwd>/<timestamp>_<sessionId>.jsonl`
    /// transcripts. When absent the transcript scan is skipped.
    pub sessions_directory: Option<PathBuf>,
    /// Environment override of the threshold knob; defaults to the process environment.
    pub env: Option<BTreeMap<String, String>>,
}

/// One work demoted by [`crate::reconcile_stale_works`] (`StaleWorkDemotion`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaleWorkDemotion {
    pub work_id: String,
    pub stale_since: String,
    pub last_activity_at: Option<String>,
}

/// Result of [`crate::reconcile_stale_works`] (`StaleWorkReconcileResult`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StaleWorkReconcileResult {
    pub demoted: Vec<StaleWorkDemotion>,
    pub written: bool,
}
