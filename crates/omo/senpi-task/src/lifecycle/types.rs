use super::errors::AgentLimitReached;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionResult {
    Admitted,
    Evicted { evicted_task_id: String },
    Rejected(AgentLimitReached),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReconcileOutcomeKind {
    Resumed,
    Lost,
    LostAndTerminated,
    ForeignLiveOwner,
    Deferred,
}

impl ReconcileOutcomeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resumed => "resumed",
            Self::Lost => "lost",
            Self::LostAndTerminated => "lost_and_terminated",
            Self::ForeignLiveOwner => "foreign_live_owner",
            Self::Deferred => "deferred",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileOutcome {
    pub task_id: String,
    pub kind: ReconcileOutcomeKind,
    pub reason: Option<String>,
}

impl ReconcileOutcome {
    pub fn new(task_id: &str, kind: ReconcileOutcomeKind, reason: Option<&str>) -> Self {
        Self {
            task_id: task_id.to_string(),
            kind,
            reason: reason.map(str::to_string),
        }
    }

    pub fn deferred(task_id: &str, reason: &str) -> Self {
        Self::new(task_id, ReconcileOutcomeKind::Deferred, Some(reason))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReconcileResult {
    pub outcomes: Vec<ReconcileOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CleanupResult {
    pub deleted: Vec<String>,
    pub retained: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspendInput {
    pub parent_session_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspendFailure {
    pub task_id: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SuspendSummary {
    pub suspended_in_process: usize,
    pub suspended_rpc: usize,
    pub suspended_pending: usize,
    pub disposed: usize,
    pub failures: Vec<SuspendFailure>,
}
