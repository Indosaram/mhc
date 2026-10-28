//! Native in-process hold adapter (`in-process/native-run-pause.ts`).
use crate::pause::{
    native_child_pause::{NativePauseError, NativeResumeResult, NativeRunPause},
    ports::*,
};
use std::sync::Arc;

pub struct EngineTurnWatch {
    pub outcome: Box<dyn FnOnce() -> crate::runners::RunnerOutcome + Send>,
    pub cancel: Box<dyn FnOnce() + Send>,
}
pub type NativePauseView = (Option<u64>, Vec<(String, Option<String>)>);
pub trait RunHoldSession: Send + Sync {
    fn pause_for_run(&self, owner: RunPauseOwner)
    -> Result<EngineInheritReceipt, NativePauseError>;
    fn resume_for_run(
        &self,
        request: ResumeTurnRequest,
    ) -> Result<EngineResumeReceipt, NativePauseError>;
    fn hold_input(&self, input: HoldInput) -> Result<EngineHoldInputResult, NativePauseError>;
    fn pause_view(&self) -> Option<NativePauseView>;
}
pub struct InProcessRunPause {
    session: Arc<dyn RunHoldSession>,
    watch_engine_turn: Arc<dyn Fn() -> EngineTurnWatch + Send + Sync>,
}
pub fn create_in_process_run_pause(
    session: Arc<dyn RunHoldSession>,
    watch_engine_turn: Arc<dyn Fn() -> EngineTurnWatch + Send + Sync>,
) -> InProcessRunPause {
    InProcessRunPause {
        session,
        watch_engine_turn,
    }
}
impl NativeRunPause for InProcessRunPause {
    fn pause_for_run(
        &self,
        owner: RunPauseOwner,
    ) -> Result<EngineInheritReceipt, NativePauseError> {
        self.session.pause_for_run(owner)
    }
    fn resume_for_run(
        &self,
        request: ResumeTurnRequest,
    ) -> Result<NativeResumeResult, NativePauseError> {
        let watch = (self.watch_engine_turn)();
        match self.session.resume_for_run(request) {
            Ok(EngineResumeReceipt::Released {
                turn_id: Some(turn_id),
                ..
            }) => Ok(NativeResumeResult::Turn(ResumedChildTurn {
                turn_id,
                outcome: watch.outcome,
            })),
            Ok(EngineResumeReceipt::Released { turn_id: None, .. }) => {
                (watch.cancel)();
                Ok(NativeResumeResult::OtherHoldLive)
            }
            Ok(EngineResumeReceipt::Refused(reason)) => {
                (watch.cancel)();
                Ok(NativeResumeResult::Refused(reason))
            }
            Err(error) => {
                (watch.cancel)();
                Err(error)
            }
        }
    }
    fn hold_input(
        &self,
        parent: &str,
        input: HoldInput,
    ) -> Result<EngineHoldInputResult, NativePauseError> {
        let owned = self.session.pause_view().is_some_and(|(_, records)| {
            records
                .iter()
                .any(|(kind, owner)| kind == "inherited_hold" && owner.as_deref() == Some(parent))
        });
        if !owned {
            return Err(NativePauseError::Engine("wrong_parent".into()));
        }
        self.session.hold_input(input)
    }
    fn observe_hold(&self, parent: &str) -> Result<Option<ObservedHold>, NativePauseError> {
        let Some((Some(child_seq), records)) = self.session.pause_view() else {
            return Ok(None);
        };
        Ok(Some(ObservedHold {
            child_seq,
            held: records
                .iter()
                .any(|(kind, owner)| kind == "inherited_hold" && owner.as_deref() == Some(parent)),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Session;
    impl RunHoldSession for Session {
        fn pause_for_run(
            &self,
            _: RunPauseOwner,
        ) -> Result<EngineInheritReceipt, NativePauseError> {
            Ok(EngineInheritReceipt::Inherited { child_seq: 2 })
        }
        fn resume_for_run(
            &self,
            _: ResumeTurnRequest,
        ) -> Result<EngineResumeReceipt, NativePauseError> {
            Ok(EngineResumeReceipt::Released {
                other_hold_live: false,
                turn_id: Some("own-turn".into()),
            })
        }
        fn hold_input(&self, _: HoldInput) -> Result<EngineHoldInputResult, NativePauseError> {
            Ok(EngineHoldInputResult::AlreadyDelivered { revision: 1 })
        }
        fn pause_view(&self) -> Option<(Option<u64>, Vec<(String, Option<String>)>)> {
            Some((None, vec![("inherited_hold".into(), Some("parent".into()))]))
        }
    }
    #[test]
    fn missing_sequence_is_unknown_and_wrong_parent_cannot_hold_input() {
        let pause = create_in_process_run_pause(
            Arc::new(Session),
            Arc::new(|| EngineTurnWatch {
                outcome: Box::new(|| crate::runners::RunnerOutcome::completed("own result")),
                cancel: Box::new(|| {}),
            }),
        );
        assert!(pause.observe_hold("parent").unwrap().is_none());
        assert!(
            pause
                .hold_input(
                    "foreign",
                    HoldInput {
                        input_id: "g".into(),
                        revision: 1,
                        text: "guide".into()
                    }
                )
                .is_err()
        );
        let result = pause
            .resume_for_run(ResumeTurnRequest {
                owner: RunPauseOwner {
                    parent_session_id: "parent".into(),
                    parent_pause_seq: 1,
                    run_ref: RunRef::task("task"),
                },
                resume_id: "resume".into(),
                child_seq_cutoff: 2,
            })
            .unwrap();
        let NativeResumeResult::Turn(turn) = result else {
            panic!("expected turn")
        };
        assert_eq!(turn.turn_id, "own-turn");
        assert_eq!(
            (turn.outcome)(),
            crate::runners::RunnerOutcome::completed("own result")
        );
    }
}
