use maho_ext_api::{EventKind, EventResult, ExtensionApi};
use std::sync::{Arc, Mutex, PoisonError};
pub const TASK_USAGE_HINT_FLAG: &str = "omo-task-usage-hint";
pub fn wire_task_usage_guidance(
    api: &mut ExtensionApi,
    enabled: Arc<dyn Fn() -> bool + Send + Sync>,
) {
    let guard = Arc::new(Mutex::new(
        crate::usage_guidance::OncePerSessionGuard::default(),
    ));
    api.on(
        EventKind::BeforeAgentStart,
        Arc::new(move |_, context| {
            let first = enabled()
                && guard
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .first_delivery(context.session_manager.session_id());
            Box::pin(async move {
                Ok(if first {
                    EventResult::BeforeAgentStart(maho_ext_api::BeforeAgentStartEventResult {
                        system_prompt_parts: None,
                        message: Some(maho_ext_api::CustomMessage {
                            custom_type: "senpi-task.usage".into(),
                            content: vec![maho_ext_api::ToolContent::text(
                                crate::usage_guidance::TASK_USAGE_GUIDANCE,
                            )],
                            display: false,
                            details: Some(serde_json::json!({})),
                        }),
                        system_prompt: None,
                    })
                } else {
                    EventResult::None
                })
            })
        }),
    );
}

/// The pause custody the component restores BEFORE its lifecycle reconcile and applies on the
/// host's Resume edge. `event-bridge.ts` owns this seam: custody must exist before the reconcile
/// reads records, or a restored Stop looks like an ordinary running session.
#[derive(Clone)]
pub struct TaskPauseCustody {
    pub bridge: std::sync::Arc<crate::run_pause_bridge::EngineRunPauseBridge>,
    pub session_id: std::sync::Arc<dyn Fn() -> Option<String> + Send + Sync>,
}

impl TaskPauseCustody {
    /// Restore the durable Stop (if any) for the current session. Returns the restored pause sequence.
    pub fn restore(&self) -> Option<u64> {
        (self.session_id)().and_then(|session| self.bridge.restore(&session))
    }

    /// Apply one host hold change, after validating it against the restored custody.
    pub fn apply(
        &self,
        event: &crate::run_pause_bridge::RunHoldChangeEvent,
    ) -> crate::run_pause_bridge::BridgeOutcome {
        match (self.session_id)() {
            Some(session) => self.bridge.apply(&session, event),
            None => crate::run_pause_bridge::BridgeOutcome::Ignored,
        }
    }

    /// The host released its held wakes (real input or an explicit Resume): resume the restored Stop.
    /// The pause sequence comes from the RESTORED record, never from the event payload, so an old
    /// Resume can never reopen admission past a newer Stop.
    pub fn resume_current(&self) -> crate::run_pause_bridge::BridgeOutcome {
        let Some(pause_seq) = self.restore() else {
            return crate::run_pause_bridge::BridgeOutcome::Ignored;
        };
        self.apply(&crate::run_pause_bridge::RunHoldChangeEvent {
            state: crate::engine_hold_reader::EngineHoldState::Rpc(
                crate::engine_hold_reader::RpcPauseState {
                    pause_seq,
                    records: Vec::new(),
                },
            ),
            change: crate::run_pause_bridge::RunHoldChange::Resumed,
        })
    }

    /// Observe the native host session bus. Run fences also emit `session_paused`, but only a
    /// session hold is a Stop; applying fences must not recursively pause the controller.
    pub fn register(&self, api: &mut maho_ext_api::ExtensionApi) -> maho_ext_api::BusSubscription {
        let pending = Arc::new(Mutex::new(Vec::<std::thread::JoinHandle<()>>::new()));
        let stopped = pending.clone();
        let custody = self.clone();
        let stop = api.events.on(
            "session:hold",
            Arc::new(move |payload| {
                if payload["type"] != "session_paused" || payload["kind"] != "session_hold" {
                    return;
                }
                let Some(pause_seq) = payload["pauseSeq"].as_u64() else {
                    return;
                };
                let Some(session) = (custody.session_id)() else {
                    return;
                };
                custody.bridge.close_admission(&session, pause_seq);
                // The bus is emitted while the host hold ledger is locked. Propagation calls back
                // into that ledger to add run fences, so it must run after the emitter returns.
                let custody = custody.clone();
                let propagation = std::thread::spawn(move || {
                    let outcome = custody.bridge.apply(
                        &session,
                        &crate::run_pause_bridge::RunHoldChangeEvent {
                            state: crate::engine_hold_reader::EngineHoldState::Rpc(
                                crate::engine_hold_reader::RpcPauseState {
                                    pause_seq,
                                    records: vec![crate::engine_hold_reader::RpcPauseRecord {
                                        kind: "session_hold".into(),
                                        key: "session_hold".into(),
                                        run_ref: None,
                                        parent_session_id: None,
                                    }],
                                },
                            ),
                            change: crate::run_pause_bridge::RunHoldChange::Paused,
                        },
                    );
                    if let crate::run_pause_bridge::BridgeOutcome::Failed(error) = outcome {
                        maho_ext_api::diagnostic(&format!("run Stop propagation failed: {error}"));
                    }
                });
                stopped
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(propagation);
            }),
        );
        let custody = self.clone();
        api.on(
            maho_ext_api::EventKind::HeldWakesReleased,
            std::sync::Arc::new(move |_, _| {
                let custody = custody.clone();
                let pending = pending.clone();
                Box::pin(async move {
                    let propagations = std::mem::take(
                        &mut *pending.lock().unwrap_or_else(PoisonError::into_inner),
                    );
                    for propagation in propagations {
                        if propagation.join().is_err() {
                            return Err(maho_ext_api::ExtensionFailure::new(
                                "run Stop propagation panicked",
                            ));
                        }
                    }
                    let _ = custody.resume_current();
                    Ok(maho_ext_api::EventResult::None)
                })
            }),
        );
        stop
    }
}

/// The task id this process may claim as its own on shutdown: the hosted child's identity, trusted
/// only when its canonical `state_dir` is exactly this engine's `<store>/children/<task_id>`.
pub fn own_task_identity_for_shutdown(
    session_context: &std::collections::BTreeMap<String, String>,
    engine_state_dir: &std::path::Path,
) -> Option<String> {
    let context = serde_json::to_value(session_context).ok()?;
    crate::own_task_identity::own_hosted_task_id(Some(&context), engine_state_dir)
}

/// The session id a shutdown must suspend: the hosted child's own task id when it is proven to be
/// ours, else the live session id. A foreign or unprovable context never narrows the scope.
pub fn shutdown_owner(
    session: &str,
    session_context: &std::collections::BTreeMap<String, String>,
    engine_state_dir: &std::path::Path,
) -> String {
    own_task_identity_for_shutdown(session_context, engine_state_dir)
        .unwrap_or_else(|| session.to_string())
}

/// Whether a child's failed stop must be reported: only a child that actually started is worth a
/// warning, and a stopped OWNER can never be resumed, so it is never announced as resumable.
pub fn shutdown_notice(started: bool, is_own_task: bool) -> Option<&'static str> {
    match (started, is_own_task) {
        (false, _) => None,
        (true, true) => {
            Some("this hosted task could not be suspended; it cannot be resumed automatically")
        }
        (true, false) => Some("a task child could not be suspended; it continues in this session"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn a_foreign_or_missing_context_keeps_the_live_session_as_the_shutdown_owner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let empty = BTreeMap::new();
        assert_eq!(shutdown_owner("session-1", &empty, dir.path()), "session-1");
        let mut context = BTreeMap::new();
        context.insert("task_id".to_string(), "st_1".to_string());
        context.insert(
            "state_dir".to_string(),
            "/somewhere/else/children/st_1".to_string(),
        );
        assert_eq!(
            shutdown_owner("session-1", &context, dir.path()),
            "session-1"
        );
    }

    #[test]
    fn a_proven_own_task_identity_narrows_the_shutdown_owner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = dir.path().join("store");
        std::fs::create_dir_all(&store).expect("store");
        let mut context = BTreeMap::new();
        context.insert("task_id".to_string(), "st_7".to_string());
        context.insert(
            "state_dir".to_string(),
            store
                .join("children")
                .join("st_7")
                .to_string_lossy()
                .into_owned(),
        );
        assert_eq!(shutdown_owner("session-1", &context, &store), "st_7");
    }

    #[test]
    fn a_child_that_never_started_is_not_warned_about() {
        assert_eq!(shutdown_notice(false, false), None);
        assert!(
            shutdown_notice(true, true).is_some_and(|notice| notice.contains("cannot be resumed"))
        );
        assert!(shutdown_notice(true, false).is_some_and(|notice| notice.contains("continues")));
    }
}
