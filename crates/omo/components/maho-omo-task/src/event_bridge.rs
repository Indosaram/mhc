use maho_ext_api::{EventKind, EventResult, ExtensionApi};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::watch;
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

/// Ends a task whose Resume never brought it back.
pub type FailResume = dyn Fn(&str) -> Result<(), String> + Send + Sync;

/// The pause custody the component restores BEFORE its lifecycle reconcile and applies on the
/// host's Resume edge. `event-bridge.ts` owns this seam: custody must exist before the reconcile
/// reads records, or a restored Stop looks like an ordinary running session.
#[derive(Clone)]
pub struct TaskPauseCustody {
    pub bridge: std::sync::Arc<crate::run_pause_bridge::EngineRunPauseBridge>,
    pub session_id: std::sync::Arc<dyn Fn() -> Option<String> + Send + Sync>,
    /// Gives a task an explicit terminal state when a Resume never brought it back. `None` keeps the
    /// diagnostic-only behavior for hosts without a task manager.
    pub fail_resume: Option<std::sync::Arc<FailResume>>,
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
            std::sync::Arc::new(move |_, ctx| {
                let custody = custody.clone();
                let pending = pending.clone();
                let ui = ctx.has_ui.then(|| ctx.ui.clone());
                Box::pin(async move {
                    // The join and the Resume block, and the Resume waits on child turns the host
                    // runtime must drive (a native child admits its continue prompt on that same
                    // runtime). Blocking the handler's own thread would starve that runtime when
                    // it is current_thread, so the blocking work runs off it and is awaited.
                    let work = move || {
                        let propagations = std::mem::take(
                            &mut *pending.lock().unwrap_or_else(PoisonError::into_inner),
                        );
                        // Two closures need the custody: the Resume borrows the bridge, the
                        // escalation borrows the task-writer. Split them before either moves.
                        let fail_resume = custody.fail_resume.clone();
                        finish_held_wakes_reported(
                            propagations,
                            move || custody.resume_current(),
                            move |failed: &[String]| {
                                escalate_failed_resumes(
                                    failed,
                                    fail_resume.as_deref(),
                                    ui.as_deref(),
                                )
                            },
                        )
                    };
                    match tokio::runtime::Handle::try_current() {
                        Ok(runtime) => runtime.spawn_blocking(work).await.map_err(|error| {
                            maho_ext_api::ExtensionFailure::new(format!(
                                "run Resume worker failed: {error}"
                            ))
                        })??,
                        Err(_) => work()?,
                    }
                    Ok(maho_ext_api::EventResult::None)
                })
            }),
        );
        stop
    }
}

/// Give every task a Resume left behind an explicit terminal state. Without this the record stays
/// `Paused` with nobody owning the wake, and the user only sees a run that never continues.
fn escalate_failed_resumes(
    failed_keys: &[String],
    fail_resume: Option<&FailResume>,
    ui: Option<&dyn maho_ext_api::ExtensionUi>,
) {
    let Some(fail_resume) = fail_resume else {
        return;
    };
    let mut stranded = 0usize;
    for key in failed_keys {
        match fail_resume(key) {
            Ok(()) => stranded += 1,
            Err(error) => {
                stranded += 1;
                maho_ext_api::diagnostic(&format!(
                    "task {key} could not be resumed and could not be interrupted: {error}"
                ));
            }
        }
    }
    if let Some(ui) = ui
        && stranded > 0
    {
        ui.notify(
            &format!(
                "{stranded} paused task(s) could not be resumed and were ended; start them again to continue"
            ),
            maho_ext_api::NotificationType::Warning,
        );
    }
}

/// Join every held-wake propagation, then resume unconditionally: a panicked propagation must never
/// leave the run Paused. Panics are reported only after the Resume had its chance.
///
/// A partial Resume is NOT just a diagnostic: participants that stayed outside custody would keep
/// their tasks Paused with nobody left to wake them, which is indistinguishable from a hang. The
/// caller passes `escalate` to give exactly those tasks an explicit terminal state and tell the user.
/// A test-only convenience: every production caller also escalates failed Resume participants, so the
/// two-argument form exists only to keep the panic/retry tests readable.
#[cfg(test)]
fn finish_held_wakes(
    propagations: Vec<std::thread::JoinHandle<()>>,
    resume: impl Fn() -> crate::run_pause_bridge::BridgeOutcome,
) -> Result<(), maho_ext_api::ExtensionFailure> {
    finish_held_wakes_reported(propagations, resume, |_| {})
}

fn finish_held_wakes_reported(
    propagations: Vec<std::thread::JoinHandle<()>>,
    resume: impl Fn() -> crate::run_pause_bridge::BridgeOutcome,
    mut escalate: impl FnMut(&[String]),
) -> Result<(), maho_ext_api::ExtensionFailure> {
    let mut panicked = 0usize;
    for propagation in propagations {
        if propagation.join().is_err() {
            panicked += 1;
        }
    }
    let mut outcome = resume();
    // One bounded retry, only while admission is still closed: the bridge's sequence admits a
    // repeated Resume, the controller skips participants it already resumed, and re-running the
    // guidance dispatch is idempotent, so the retry only redoes the work that was lost.
    if resume_retryable(&outcome) {
        outcome = resume();
    }
    match outcome {
        crate::run_pause_bridge::BridgeOutcome::Failed(error) => {
            maho_ext_api::diagnostic(&format!("run Resume propagation failed: {error}"));
        }
        // A Resume can install and still leave participants behind, or fail to release admission at
        // all; both are partial failures the caller never sees otherwise, because the outcome is a
        // success variant.
        crate::run_pause_bridge::BridgeOutcome::Resumed {
            installed,
            failures,
            failed_keys,
        } if !installed || failures > 0 => {
            maho_ext_api::diagnostic(&format!(
                "run Resume propagated with {failures} failed participant(s), admission released: {installed}"
            ));
            escalate(&failed_keys);
        }
        _ => {}
    }
    if panicked > 0 {
        return Err(maho_ext_api::ExtensionFailure::new(format!(
            "{panicked} run Stop propagation(s) panicked"
        )));
    }
    Ok(())
}

/// Whether one retry can still resume something. `Resumed { installed: true, .. }` means admission
/// was already released, so the retry could only die at `AdmissionBarrier::resume` on an open
/// barrier and would replace the accurate partial-failure diagnostic with a stale-sequence one;
/// those participants need the bridge's `retry_lost`, not a second Resume.
fn resume_retryable(outcome: &crate::run_pause_bridge::BridgeOutcome) -> bool {
    matches!(
        outcome,
        crate::run_pause_bridge::BridgeOutcome::Failed(_)
            | crate::run_pause_bridge::BridgeOutcome::Resumed {
                installed: false,
                ..
            }
    )
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
    fn a_panicked_stop_propagation_still_resumes_and_reports_the_panic_count() {
        let propagations = vec![
            std::thread::spawn(|| panic!("propagation failed")),
            std::thread::spawn(|| {}),
        ];
        let resumes = std::sync::atomic::AtomicUsize::new(0);
        let result = finish_held_wakes(propagations, || {
            resumes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            crate::run_pause_bridge::BridgeOutcome::Resumed {
                installed: true,
                failures: 0,
                failed_keys: Vec::new(),
            }
        });
        assert_eq!(resumes.load(std::sync::atomic::Ordering::SeqCst), 1);
        let error = result.expect_err("the panics are reported");
        assert!(
            error.message.contains("1 run Stop propagation(s) panicked"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_failed_resume_is_retried_exactly_once() {
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let result = finish_held_wakes(Vec::new(), || {
            if attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                crate::run_pause_bridge::BridgeOutcome::Failed("host refused".into())
            } else {
                crate::run_pause_bridge::BridgeOutcome::Resumed {
                    installed: true,
                    failures: 0,
                    failed_keys: Vec::new(),
                }
            }
        });
        assert!(result.is_ok());
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn a_resume_that_never_released_admission_is_retried_once() {
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        assert!(
            finish_held_wakes(Vec::new(), || {
                attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                crate::run_pause_bridge::BridgeOutcome::Resumed {
                    installed: false,
                    failures: 1,
                    failed_keys: vec!["run".into()],
                }
            })
            .is_ok()
        );
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    /// Admission is already open once `installed` is true, so a second Resume would only fail
    /// authorization; the partial-failure diagnostic is the accurate report for that outcome.
    #[test]
    fn a_resume_that_released_admission_is_never_retried() {
        for outcome in [
            crate::run_pause_bridge::BridgeOutcome::Resumed {
                installed: true,
                failures: 0,
                failed_keys: Vec::new(),
            },
            crate::run_pause_bridge::BridgeOutcome::Resumed {
                installed: true,
                failures: 2,
                failed_keys: vec!["a".into(), "b".into()],
            },
        ] {
            let attempts = std::sync::atomic::AtomicUsize::new(0);
            assert!(
                finish_held_wakes(Vec::new(), || {
                    attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    outcome.clone()
                })
                .is_ok()
            );
            assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
        }
    }

    /// A participant a Resume left behind must not stay `Paused` with nobody owning the wake: the
    /// failed keys are handed to the escalation port exactly once, after the retry gave up.
    #[test]
    fn a_partial_resume_hands_every_stranded_participant_to_escalation() {
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let mut escalated: Vec<String> = Vec::new();
        let result = finish_held_wakes_reported(
            Vec::new(),
            || {
                attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                crate::run_pause_bridge::BridgeOutcome::Resumed {
                    installed: true,
                    failures: 2,
                    failed_keys: vec!["task-a".into(), "task-b".into()],
                }
            },
            |failed| escalated.extend(failed.iter().cloned()),
        );
        assert!(result.is_ok());
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(escalated, vec!["task-a".to_string(), "task-b".to_string()]);
    }

    /// A clean Resume must not touch any task record.
    #[test]
    fn a_clean_resume_never_escalates() {
        let mut escalated: Vec<String> = Vec::new();
        assert!(
            finish_held_wakes_reported(
                Vec::new(),
                || crate::run_pause_bridge::BridgeOutcome::Resumed {
                    installed: true,
                    failures: 0,
                    failed_keys: Vec::new(),
                },
                |failed| escalated.extend(failed.iter().cloned()),
            )
            .is_ok()
        );
        assert!(escalated.is_empty());
    }

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

/// One session start's recovery state (`event-bridge.ts` `startup` controller + `reconciledGate`).
pub struct StartState {
    active: std::sync::atomic::AtomicBool,
    reconciled: watch::Sender<Option<bool>>,
    recovered: watch::Sender<bool>,
}

impl StartState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            active: std::sync::atomic::AtomicBool::new(true),
            reconciled: watch::channel(None).0,
            recovered: watch::channel(false).0,
        })
    }

    /// False once a newer start, a switch or a shutdown retired this start.
    pub fn active(&self) -> bool {
        self.active.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Settles the reconcile gate with whether this start is still current; the first call wins.
    pub fn settle_reconciled(&self) {
        let current = self.active();
        self.reconciled.send_if_modified(|value| {
            value.is_none() && {
                *value = Some(current);
                true
            }
        });
    }

    /// Marks the whole recovery chain as ended.
    pub fn finish(&self) {
        self.recovered.send_replace(true);
    }

    fn retire(&self) {
        self.active
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.settle_reconciled();
    }
}

/// What the task event bridge hands to the components that must run after its reconcile.
#[derive(Clone)]
pub struct EventBridge(Arc<Mutex<Arc<StartState>>>);

impl Default for EventBridge {
    fn default() -> Self {
        // Before any start the gate is open and current, like `Promise.resolve(true)`.
        let initial = StartState::new();
        initial.settle_reconciled();
        initial.finish();
        Self(Arc::new(Mutex::new(initial)))
    }
}

impl EventBridge {
    /// Retires the previous start and opens a new one.
    pub fn begin(&self) -> Arc<StartState> {
        let next = StartState::new();
        let previous = std::mem::replace(
            &mut *self.0.lock().unwrap_or_else(PoisonError::into_inner),
            next.clone(),
        );
        previous.retire();
        next
    }

    /// Retires the current start (session switch or shutdown).
    pub fn abort(&self) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retire();
    }

    fn current(&self) -> Arc<StartState> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Settles when the latest session start's task reconcile finished; false when a newer start superseded it.
    pub async fn after_session_start_reconcile(&self) -> bool {
        let mut receiver = self.current().reconciled.subscribe();
        receiver
            .wait_for(Option::is_some)
            .await
            .ok()
            .and_then(|value| *value)
            .unwrap_or(false)
    }

    /// Settles when the latest session start's whole recovery chain ended.
    pub async fn session_start_recovered(&self) {
        let mut receiver = self.current().recovered.subscribe();
        let _ = receiver.wait_for(|done| *done).await;
    }
}
