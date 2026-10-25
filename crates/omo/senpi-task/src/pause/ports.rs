//! Run pause ports (`pause/ports.ts`).

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::admission_barrier::AdmissionTicket;
use crate::host::HostError;
use crate::manager::child_handle::ManagedChildHandle;
use crate::manager::types::{ManagedRunner, ManagedRunnerError, ManagedStartSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunRefKind {
    Dag,
    Task,
    Workpool,
    Team,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPauseOwner {
    pub parent_session_id: String,
    pub parent_pause_seq: u64,
    pub run_ref: RunRef,
}

/// A turn's outcome remains deferred until the caller waits, matching the blocking child seam.
pub struct ResumedChildTurn {
    pub turn_id: String,
    pub outcome: Box<dyn FnOnce() -> crate::runners::RunnerOutcome + Send>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineInheritReceipt {
    Inherited { child_seq: u64 },
    WrongParent,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedHold {
    pub held: bool,
    pub child_seq: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeTurnRequest {
    pub owner: RunPauseOwner,
    pub resume_id: String,
    pub child_seq_cutoff: u64,
}
pub enum ResumeChildTurnResult {
    Resumed(ResumedChildTurn),
    OtherHoldLive { child_seq: Option<u64> },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldInput {
    pub input_id: String,
    pub revision: u64,
    pub text: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineHoldInputResult {
    Held { entry_id: String, duplicate: bool },
    AlreadyDelivered { revision: u64 },
    InputIdConflict { held_revision: u64 },
}

pub trait ChildPausePort: Send + Sync {
    fn pause_for_run(
        &self,
        child: &dyn ManagedChildHandle,
        owner: &RunPauseOwner,
    ) -> Result<EngineInheritReceipt, HostError>;
    fn observe_hold(
        &self,
        _child: &dyn ManagedChildHandle,
        _parent_session_id: &str,
    ) -> Result<Option<ObservedHold>, HostError> {
        Ok(None)
    }
    fn resume_turn(
        &self,
        child: &dyn ManagedChildHandle,
        request: &ResumeTurnRequest,
    ) -> Result<ResumeChildTurnResult, HostError>;
    fn hold_input(
        &self,
        child: &dyn ManagedChildHandle,
        input: &HoldInput,
        parent_session_id: &str,
    ) -> Result<EngineHoldInputResult, HostError>;
}

pub enum GuardedChildActivation {
    Resumed(ResumedChildTurn),
    Paused {
        child_seq: u64,
        owner: RunPauseOwner,
    },
}
pub struct GuardedChild {
    pub child: Arc<dyn ManagedChildHandle>,
    pub activate: Box<dyn FnOnce() -> Result<GuardedChildActivation, ManagedRunnerError> + Send>,
}
pub trait ChildCreationPort: Send + Sync {
    /// Return an unstarted child; activation performs the final admission check before prompting.
    fn start_paused(
        &self,
        spec: &ManagedStartSpec,
        runner: &dyn ManagedRunner,
        ticket: AdmissionTicket,
    ) -> Result<GuardedChild, ManagedRunnerError>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineHold {
    pub kind: String,
    pub key: String,
    pub data: Option<serde_json::Value>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EnginePauseState {
    pub pause_seq: u64,
    pub holds: Vec<EngineHold>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeRefusal {
    Stale,
    NotHeld,
    WrongParent,
}
impl std::fmt::Display for ResumeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Stale => "stale",
            Self::NotHeld => "not_held",
            Self::WrongParent => "wrong_parent",
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineResumeReceipt {
    Refused(ResumeRefusal),
    Released {
        other_hold_live: bool,
        turn_id: Option<String>,
    },
}
pub const RELEASE_RUN_FENCE: &str = "releaseRunFence";
pub trait EngineRunHoldContext: Send + Sync {
    fn get_pause_state(&self) -> Option<EnginePauseState> {
        None
    }
    fn fence_run(&self, _run_ref: &RunRef) {}
    fn release_run_fence(&self, _run_ref: &RunRef) {}
}
#[derive(Debug, Clone, PartialEq)]
pub struct EngineHoldChangedEvent {
    pub state: EnginePauseState,
    pub change: HoldChange,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldChange {
    Paused,
    Resumed,
}
