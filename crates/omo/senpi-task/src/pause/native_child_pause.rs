//! Native engine adapter (`pause/native-child-pause.ts`). No local pause simulation.
use super::ports::*;
use crate::manager::ManagedChildHandle;

pub enum NativeResumeResult {
    Refused(ResumeRefusal),
    OtherHoldLive,
    Turn(ResumedChildTurn),
}
pub trait NativeRunPause: Send + Sync {
    fn pause_for_run(&self, owner: RunPauseOwner)
    -> Result<EngineInheritReceipt, NativePauseError>;
    fn resume_for_run(
        &self,
        request: ResumeTurnRequest,
    ) -> Result<NativeResumeResult, NativePauseError>;
    fn hold_input(
        &self,
        parent: &str,
        input: HoldInput,
    ) -> Result<EngineHoldInputResult, NativePauseError>;
    fn observe_hold(&self, _parent: &str) -> Result<Option<ObservedHold>, NativePauseError> {
        Ok(None)
    }
}
#[derive(Debug, thiserror::Error)]
pub enum NativePauseError {
    #[error("child {task_id} runs on an engine without native run pause")]
    Unavailable { task_id: String },
    #[error("child {task_id} refused the run Resume: {reason}")]
    Refused {
        task_id: String,
        reason: ResumeRefusal,
    },
    #[error("{0}")]
    Engine(String),
}
impl NativePauseError {
    pub fn code(&self) -> &str {
        match self {
            Self::Unavailable { .. } => "native_pause_unavailable",
            Self::Refused { .. } => "native_resume_refused",
            Self::Engine(_) => "engine",
        }
    }
}
fn native(child: &dyn ManagedChildHandle) -> Result<&dyn NativeRunPause, NativePauseError> {
    child
        .run_pause()
        .ok_or_else(|| NativePauseError::Unavailable {
            task_id: child.task_id().into(),
        })
}
pub struct NativeChildPausePort;
impl NativeChildPausePort {
    pub fn pause_for_run(
        &self,
        child: &dyn ManagedChildHandle,
        owner: RunPauseOwner,
    ) -> Result<EngineInheritReceipt, NativePauseError> {
        native(child)?.pause_for_run(owner)
    }
    pub fn resume_turn(
        &self,
        child: &dyn ManagedChildHandle,
        request: ResumeTurnRequest,
    ) -> Result<ResumeChildTurnResult, NativePauseError> {
        match native(child)?.resume_for_run(request)? {
            NativeResumeResult::Refused(reason) => Err(NativePauseError::Refused {
                task_id: child.task_id().into(),
                reason,
            }),
            NativeResumeResult::OtherHoldLive => {
                Ok(ResumeChildTurnResult::OtherHoldLive { child_seq: None })
            }
            NativeResumeResult::Turn(turn) => Ok(ResumeChildTurnResult::Resumed(turn)),
        }
    }
    pub fn hold_input(
        &self,
        child: &dyn ManagedChildHandle,
        input: HoldInput,
        parent: &str,
    ) -> Result<EngineHoldInputResult, NativePauseError> {
        native(child)?.hold_input(parent, input)
    }
    pub fn observe_hold(
        &self,
        child: &dyn ManagedChildHandle,
        parent: &str,
    ) -> Result<Option<ObservedHold>, NativePauseError> {
        match child.run_pause() {
            Some(engine) => engine.observe_hold(parent),
            None => Ok(None),
        }
    }
}
pub const NATIVE_CHILD_PAUSE_PORT: NativeChildPausePort = NativeChildPausePort;

impl ChildPausePort for NativeChildPausePort {
    fn pause_for_run(
        &self,
        child: &dyn ManagedChildHandle,
        owner: &RunPauseOwner,
    ) -> Result<EngineInheritReceipt, crate::host::HostError> {
        NativeChildPausePort::pause_for_run(self, child, owner.clone()).map_err(host_error)
    }
    fn observe_hold(
        &self,
        child: &dyn ManagedChildHandle,
        parent: &str,
    ) -> Result<Option<ObservedHold>, crate::host::HostError> {
        NativeChildPausePort::observe_hold(self, child, parent).map_err(host_error)
    }
    fn resume_turn(
        &self,
        child: &dyn ManagedChildHandle,
        request: &ResumeTurnRequest,
    ) -> Result<ResumeChildTurnResult, crate::host::HostError> {
        NativeChildPausePort::resume_turn(self, child, request.clone()).map_err(host_error)
    }
    fn hold_input(
        &self,
        child: &dyn ManagedChildHandle,
        input: &HoldInput,
        parent: &str,
    ) -> Result<EngineHoldInputResult, crate::host::HostError> {
        NativeChildPausePort::hold_input(self, child, input.clone(), parent).map_err(host_error)
    }
}
fn host_error(error: NativePauseError) -> crate::host::HostError {
    crate::host::HostError {
        message: error.to_string(),
    }
}
