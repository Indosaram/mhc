//! Run pause ports (`pause/ports.ts`).

use serde::Serialize;

/// What a run belongs to. `workpool` names a pool, `team` a team run, `dag` a DAG run, and `task`
/// the task itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunRefKind {
    Dag,
    Task,
    Workpool,
    Team,
}

/// The run a handle, hold, or completion belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct RunRef {
    pub kind: RunRefKind,
    pub id: String,
}

impl RunRef {
    pub fn new(kind: RunRefKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }

    pub fn task(id: impl Into<String>) -> Self {
        Self::new(RunRefKind::Task, id)
    }
}

/// The run's pause owner: the parent session, the pause sequence it was stopped at, and the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPauseOwner {
    pub parent_session_id: String,
    pub parent_pause_seq: u64,
    pub run_ref: RunRef,
}
