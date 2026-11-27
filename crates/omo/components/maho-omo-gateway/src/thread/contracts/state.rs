//! Thread state & model contracts (`thread/contracts/state.ts`).

use serde::{Deserialize, Serialize};

/// What a thread is doing (`running`, `idle`, `inactive`, `archived`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadState {
    Running,
    Idle,
    Inactive,
    Archived,
}

impl ThreadState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Idle => "idle",
            Self::Inactive => "inactive",
            Self::Archived => "archived",
        }
    }
}

pub const THREAD_STATES: &[ThreadState] = &[
    ThreadState::Running,
    ThreadState::Idle,
    ThreadState::Inactive,
    ThreadState::Archived,
];

/// Finer activity of a held thread (`senpi#6688`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadActivity {
    Idle,
    Running,
    AwaitingApproval,
    Queued,
    Errored,
}

impl ThreadActivity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Queued => "queued",
            Self::Errored => "errored",
        }
    }
}

pub const THREAD_ACTIVITIES: &[ThreadActivity] = &[
    ThreadActivity::Idle,
    ThreadActivity::Running,
    ThreadActivity::AwaitingApproval,
    ThreadActivity::Queued,
    ThreadActivity::Errored,
];

/// The model a thread runs, as its engine names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadModel {
    pub provider: String,
    pub id: String,
}
