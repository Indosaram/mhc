use senpi_task::pause::{native_child_pause::NativePauseError, ports::*};
use std::sync::Arc;

pub(super) struct NativeRunHold {
    pub session: Arc<maho_core::agent_session::AgentSession>,
    pub executor: tokio::runtime::Handle,
}
impl NativeRunHold {
    fn execute<T: Send + 'static>(
        &self,
        operation: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
    ) -> Result<T, NativePauseError> {
        let executor = self.executor.clone();
        std::thread::spawn(move || executor.block_on(operation))
            .join()
            .map_err(|_| NativePauseError::Engine("native run hold thread panicked".into()))?
            .map_err(NativePauseError::Engine)
    }
}
impl senpi_task::runners::in_process::native_run_pause::RunHoldSession for NativeRunHold {
    fn pause_for_run(
        &self,
        owner: RunPauseOwner,
    ) -> Result<EngineInheritReceipt, NativePauseError> {
        let session = self.session.clone();
        self.execute(async move {
            session
                .pause_for_run(
                    &owner.parent_session_id,
                    owner.parent_pause_seq,
                    maho_core::run_holds::RunRef {
                        kind: match owner.run_ref.kind {
                            RunRefKind::Dag => "dag",
                            RunRefKind::Task => "task",
                            RunRefKind::Workpool => "workpool",
                            RunRefKind::Team => "team",
                        }
                        .into(),
                        id: owner.run_ref.id,
                    },
                )
                .await
        })
        .map(|receipt| match receipt {
            maho_core::run_holds::InheritResult::Inherited { child_seq } => {
                EngineInheritReceipt::Inherited { child_seq }
            }
            maho_core::run_holds::InheritResult::WrongParent => EngineInheritReceipt::WrongParent,
        })
    }
    fn resume_for_run(
        &self,
        request: ResumeTurnRequest,
    ) -> Result<EngineResumeReceipt, NativePauseError> {
        let session = self.session.clone();
        let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
        let subscription = session.subscribe(Arc::new(move |event| {
            if maho_rpc::session_binding::session_event_record(event)
                .ok()
                .is_some_and(|event| event["type"] == "agent_start")
            {
                let _ = started.send(());
            }
        }));
        self.execute(async move {
            let had_held_inputs = !session.held_inputs().is_empty();
            let receipt = session
                .resume_for_run(
                    &request.owner.parent_session_id,
                    request.owner.parent_pause_seq,
                    request.child_seq_cutoff,
                )
                .await?;
            let result = match receipt {
                maho_core::run_holds::ReleaseInheritedResult::Released { other_hold_live } => {
                    let mut turn_started = observed.try_recv().is_ok();
                    if !other_hold_live && !turn_started {
                        if !had_held_inputs {
                            session
                                .send_custom_message_content(
                                    "session-resumed".into(),
                                    maho_agent::harness::messages::CustomMessageContent::Text(
                                        maho_core::manual_continue::MANUAL_CONTINUE_DIRECTIVE
                                            .into(),
                                    ),
                                    false,
                                    None,
                                    maho_ext_api::SendMessageOptions {
                                        trigger_turn: true,
                                        deliver_as: None,
                                    },
                                )
                                .await?;
                        }
                        turn_started = tokio::time::timeout(
                            std::time::Duration::from_secs(10),
                            observed.recv(),
                        )
                        .await
                        .map_err(|_| "resumed engine turn did not start".to_string())?
                        .is_some();
                    }
                    let turn_id = if !other_hold_live && turn_started {
                        Some(request.resume_id)
                    } else {
                        None
                    };
                    EngineResumeReceipt::Released {
                        other_hold_live,
                        turn_id,
                    }
                }
                maho_core::run_holds::ReleaseInheritedResult::WrongParent => {
                    EngineResumeReceipt::Refused(ResumeRefusal::WrongParent)
                }
                maho_core::run_holds::ReleaseInheritedResult::Stale => {
                    EngineResumeReceipt::Refused(ResumeRefusal::Stale)
                }
                maho_core::run_holds::ReleaseInheritedResult::NotHeld => {
                    EngineResumeReceipt::Refused(ResumeRefusal::NotHeld)
                }
            };
            drop(subscription);
            Ok(result)
        })
    }
    fn hold_input(&self, input: HoldInput) -> Result<EngineHoldInputResult, NativePauseError> {
        let value=self.session.hold_input(serde_json::json!({"inputId":input.input_id,"revision":input.revision,"text":input.text})).map_err(NativePauseError::Engine)?;
        if let Some(revision) = value["heldRevision"].as_u64() {
            return Ok(EngineHoldInputResult::InputIdConflict {
                held_revision: revision,
            });
        }
        if let Some(revision) = value["revision"].as_u64() {
            return Ok(EngineHoldInputResult::AlreadyDelivered { revision });
        }
        let entry_id = value["entryId"]
            .as_str()
            .ok_or_else(|| NativePauseError::Engine("held input receipt has no entryId".into()))?;
        Ok(EngineHoldInputResult::Held {
            entry_id: entry_id.into(),
            duplicate: value["duplicate"].as_bool().unwrap_or(false),
        })
    }
    fn pause_view(&self) -> Option<(Option<u64>, Vec<(String, Option<String>)>)> {
        Some((
            Some(self.session.pause_seq()),
            self.session
                .holds()
                .into_iter()
                .map(|hold| {
                    (
                        hold.kind.as_str().into(),
                        hold.data
                            .and_then(|data| data["parentSessionId"].as_str().map(str::to_owned)),
                    )
                })
                .collect(),
        ))
    }
}
