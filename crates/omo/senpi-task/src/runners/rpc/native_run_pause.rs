//! `rpc/native-run-pause.ts`: coordinate engine resumes with exclusive turn claims.
use super::turn_claim::TurnClaim;
use crate::runners::RunnerOutcome;
use crate::runners::task_parent_control::{
    EngineHoldInputResult, EngineInheritReceipt, EngineResumeReceipt, HeldGuidanceInput,
    ObservedHold, ResumeRequest, RunPauseOwner, TaskParentControl, TaskParentControlError,
};
use std::sync::Arc;

pub enum NativeResumeReceipt {
    Refused {
        reason: String,
    },
    OtherHoldLive,
    Turn {
        turn_id: String,
        outcome: Arc<dyn Fn() -> RunnerOutcome + Send + Sync>,
    },
}
pub trait NativeRunPause: Send + Sync {
    fn pause_for_run(
        &self,
        owner: &RunPauseOwner,
    ) -> Result<EngineInheritReceipt, TaskParentControlError>;
    fn resume_for_run(
        &self,
        request: &ResumeRequest,
    ) -> Result<NativeResumeReceipt, TaskParentControlError>;
    fn hold_input(
        &self,
        parent: &str,
        input: &HeldGuidanceInput,
    ) -> Result<EngineHoldInputResult, TaskParentControlError>;
    fn observe_hold(&self, parent: &str) -> Result<Option<ObservedHold>, TaskParentControlError>;
}
pub struct RpcRunPause {
    pub task_id: String,
    pub control: Arc<TaskParentControl>,
    pub claims: TurnClaim,
    pub current_outcome: Arc<dyn Fn() -> RunnerOutcome + Send + Sync>,
}
pub fn create_rpc_run_pause(input: RpcRunPause) -> RpcRunPause {
    input
}

impl RpcRunPause {
    fn unsupported<T>(
        &self,
        result: Result<T, TaskParentControlError>,
    ) -> Result<T, TaskParentControlError> {
        result.map_err(|error| {
            if error.code == "unsupported" {
                TaskParentControlError::new(&format!("native_pause_unavailable: {}", self.task_id))
            } else {
                error
            }
        })
    }
}
impl NativeRunPause for RpcRunPause {
    fn pause_for_run(
        &self,
        owner: &RunPauseOwner,
    ) -> Result<EngineInheritReceipt, TaskParentControlError> {
        self.unsupported(self.control.pause_for_run(owner))
    }
    fn resume_for_run(
        &self,
        request: &ResumeRequest,
    ) -> Result<NativeResumeReceipt, TaskParentControlError> {
        let release = self.claims.claim();
        match self.unsupported(self.control.resume_for_run(request)) {
            Ok(EngineResumeReceipt::Refused { reason }) => {
                release();
                Ok(NativeResumeReceipt::Refused { reason })
            }
            Ok(EngineResumeReceipt::Released {
                other_hold_live,
                turn_id,
            }) => {
                if other_hold_live == Some(true) || turn_id.is_none() {
                    release();
                    Ok(NativeResumeReceipt::OtherHoldLive)
                } else {
                    Ok(NativeResumeReceipt::Turn {
                        turn_id: turn_id.unwrap_or_default(),
                        outcome: Arc::clone(&self.current_outcome),
                    })
                }
            }
            Err(error) => {
                release();
                Err(error)
            }
        }
    }
    fn hold_input(
        &self,
        parent: &str,
        input: &HeldGuidanceInput,
    ) -> Result<EngineHoldInputResult, TaskParentControlError> {
        self.unsupported(self.control.hold_input(parent, input))
    }
    fn observe_hold(&self, parent: &str) -> Result<Option<ObservedHold>, TaskParentControlError> {
        self.unsupported(self.control.observe_hold(parent))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runners::task_parent_control::{TaskParentWire, create_task_parent_control};
    use serde_json::json;
    #[test]
    fn resume_claims_before_agent_start_and_releases_only_its_own_claim() {
        for mode in 0..4 {
            let claims = TurnClaim::default();
            let observed = claims.clone();
            let control = create_task_parent_control(TaskParentWire::new(
                80,
                Arc::new(move |command| {
                    if command["type"] == "set_client_info" {
                        return Ok(json!({"success":true}));
                    }
                    if mode == 0 {
                        assert!(observed.consume());
                        Ok(json!({"success":true,"data":{"released":true,"turnId":"turn-2"}}))
                    } else if mode == 1 {
                        Ok(json!({"success":true,"data":{"released":false,"reason":"stale"}}))
                    } else if mode == 2 {
                        Ok(json!({"success":true,"data":{"released":true,"otherHoldLive":true}}))
                    } else {
                        Ok(json!({"success":false,"error":"Unknown command: resume_for_run"}))
                    }
                }),
            ));
            let pause = RpcRunPause {
                task_id: "task".into(),
                control: Arc::new(control),
                claims: claims.clone(),
                current_outcome: Arc::new(|| RunnerOutcome::Cancelled),
            };
            let request = ResumeRequest {
                parent_session_id: "parent".into(),
                parent_pause_seq: 2,
                resume_id: "r".into(),
                child_seq_cutoff: 8,
            };
            match (mode, pause.resume_for_run(&request)) {
                (0, Ok(NativeResumeReceipt::Turn { turn_id, outcome })) => {
                    assert_eq!(turn_id, "turn-2");
                    assert_eq!(outcome(), RunnerOutcome::Cancelled);
                }
                (1, Ok(NativeResumeReceipt::Refused { reason })) => assert_eq!(reason, "stale"),
                (2, Ok(NativeResumeReceipt::OtherHoldLive)) => {}
                (3, Err(error)) => assert_eq!(error.code, "native_pause_unavailable: task"),
                _ => panic!("unexpected resume receipt"),
            }
            assert!(!claims.consume());
        }
    }
}
