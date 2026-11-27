//! `run-pause-bridge.ts`: bridge the host's native Stop/Resume onto persistent run custody.
//!
//! The host reports a hold change; this bridge validates it (a stale epoch is ignored, not applied),
//! serializes propagation and resume, restores custody BEFORE the lifecycle reconcile runs, and
//! retries the participants a failed dispatch left outside the run's custody.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::{EventKind, EventResult, ExtensionApi};
use senpi_task::pause::{admission_barrier::ResumeCredential, ports::RunRef, registry::PausePhase};

use crate::engine_hold_reader::{EngineHoldState, EngineRunHold, read_engine_run_hold};
use crate::engine_run_pause::EngineRunPause;

/// After a released Resume left participants outside the run, they are retried after each delay;
/// whatever is still lost after the last one has its run ended (#12249).
pub const DEFAULT_LOST_PARTICIPANT_RETRY_DELAYS_MS: [u64; 3] = [5_000, 30_000, 120_000];

#[derive(Clone)]
pub struct LostParticipantRecovery {
    pub delays_ms: Vec<u64>,
    pub wait: Arc<dyn Fn(u64) + Send + Sync>,
}

impl LostParticipantRecovery {
    pub fn production() -> Self {
        Self {
            delays_ms: DEFAULT_LOST_PARTICIPANT_RETRY_DELAYS_MS.to_vec(),
            wait: Arc::new(|ms| std::thread::sleep(std::time::Duration::from_millis(ms))),
        }
    }
}

/// The hold-change event a host emits (`session_hold_changed`).
pub const SESSION_HOLD_CHANGED_EVENT: &str = "session_hold_changed";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunHoldChange {
    Paused,
    Resumed,
}

/// One native hold change: the projection, plus which way it moved.
#[derive(Debug, Clone, PartialEq)]
pub struct RunHoldChangeEvent {
    pub state: EngineHoldState,
    pub change: RunHoldChange,
}

/// `isRunHoldChange`: a payload that names the event and a legal direction.
pub fn is_run_hold_change(payload: &serde_json::Value) -> Option<RunHoldChange> {
    if payload.get("type").and_then(serde_json::Value::as_str) != Some(SESSION_HOLD_CHANGED_EVENT) {
        return None;
    }
    match payload.get("change").and_then(serde_json::Value::as_str) {
        Some("paused") => Some(RunHoldChange::Paused),
        Some("resumed") => Some(RunHoldChange::Resumed),
        _ => None,
    }
}

/// What a Resume request must carry to be applied: the sequence and the credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeRequest {
    pub pause_seq: u64,
    pub resume_id: String,
}

/// The bridge's answers, so the wiring can report them without re-deriving state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeOutcome {
    Ignored,
    Paused,
    Resumed {
        installed: bool,
        failures: usize,
        /// The participants that stayed outside custody, so the wiring can name them and give them
        /// an explicit state instead of leaving them silently paused.
        failed_keys: Vec<String>,
    },
    /// The Resume named a sequence the run is no longer stopped at.
    StaleResume,
    Failed(String),
}

/// `EngineRunPauseBridge`: serialized Stop/Resume over one engine's custody.
pub struct EngineRunPauseBridge {
    engine: Arc<EngineRunPause>,
    operation: Mutex<()>,
    last_applied: Mutex<Option<u64>>,
    in_flight_resumes: Mutex<BTreeMap<String, u64>>,
    recoveries: Mutex<BTreeSet<String>>,
    lost_recovery: Option<LostParticipantRecovery>,
}

struct InFlightResumeGuard<'a> {
    map: &'a Mutex<BTreeMap<String, u64>>,
    parent: String,
    seq: u64,
}

impl Drop for InFlightResumeGuard<'_> {
    fn drop(&mut self) {
        let mut map = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        if map.get(&self.parent) == Some(&self.seq) {
            map.remove(&self.parent);
        }
    }
}

impl EngineRunPauseBridge {
    pub fn new(engine: Arc<EngineRunPause>) -> Self {
        Self {
            engine,
            operation: Mutex::new(()),
            last_applied: Mutex::new(None),
            in_flight_resumes: Mutex::new(BTreeMap::new()),
            recoveries: Mutex::new(BTreeSet::new()),
            lost_recovery: None,
        }
    }

    pub fn with_lost_recovery(mut self, recovery: Option<LostParticipantRecovery>) -> Self {
        self.lost_recovery = recovery;
        self
    }

    pub fn close_admission(&self, parent: &str, seq: u64) {
        self.engine.barrier.close(parent, seq);
    }

    fn resume_seq(
        self_engine: &Arc<EngineRunPause>,
        operation: &Mutex<()>,
        in_flight: &Mutex<BTreeMap<String, u64>>,
        parent_session_id: &str,
        target_seq: u64,
        resume_id: &str,
    ) -> BridgeOutcome {
        in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(parent_session_id.to_owned(), target_seq);
        let _guard = InFlightResumeGuard {
            map: in_flight,
            parent: parent_session_id.to_owned(),
            seq: target_seq,
        };
        let _operation = operation.lock().unwrap_or_else(PoisonError::into_inner);
        match self_engine.resume(parent_session_id, target_seq, resume_id) {
            Ok(senpi_task::pause::run_controller::RunResumeResult::Installed { failures }) => {
                let released = self_engine.release(parent_session_id, target_seq, resume_id);
                let mut failed_keys: Vec<String> =
                    failures.iter().map(|failure| failure.key.clone()).collect();
                if !released {
                    failed_keys.extend(
                        self_engine
                            .retry_lost(parent_session_id, target_seq)
                            .into_iter()
                            .map(|failure| failure.key),
                    );
                }
                BridgeOutcome::Resumed {
                    installed: released,
                    failures: failed_keys.len(),
                    failed_keys,
                }
            }
            Ok(senpi_task::pause::run_controller::RunResumeResult::Superseded) => {
                BridgeOutcome::StaleResume
            }
            Err(error) => BridgeOutcome::Failed(error),
        }
    }

    fn maybe_spawn_lost_recovery(
        self: &Arc<Self>,
        parent_session_id: &str,
        target_seq: u64,
        outcome: &BridgeOutcome,
    ) {
        let BridgeOutcome::Resumed {
            installed: true, ..
        } = outcome
        else {
            return;
        };
        let Some(recovery) = self.lost_recovery.clone() else {
            return;
        };
        if !self.engine.has_lost(parent_session_id, target_seq) {
            return;
        }
        let mut recoveries = self
            .recoveries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !recoveries.insert(parent_session_id.to_owned()) {
            return;
        }
        drop(recoveries);
        let bridge = Arc::clone(self);
        let parent = parent_session_id.to_owned();
        if std::thread::Builder::new()
            .name("run-pause-lost-recovery".into())
            .spawn(move || {
                bridge.recover_lost_sync(&parent, target_seq, &recovery);
                bridge
                    .recoveries
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&parent);
            })
            .is_err()
        {
            self.recoveries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(parent_session_id);
        }
    }

    fn recover_lost_sync(
        &self,
        parent_session_id: &str,
        pause_seq: u64,
        recovery: &LostParticipantRecovery,
    ) {
        for delay_ms in &recovery.delays_ms {
            (recovery.wait)(*delay_ms);
            if !self.engine.has_lost(parent_session_id, pause_seq) {
                return;
            }
            let _operation = self
                .operation
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let _ = self.engine.retry_lost(parent_session_id, pause_seq);
            drop(_operation);
            if !self.engine.has_lost(parent_session_id, pause_seq) {
                return;
            }
        }
        let _operation = self
            .operation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Err(error) = self.engine.end_lost(parent_session_id, pause_seq) {
            maho_ext_api::diagnostic(&format!(
                "run resume could not end lost participants for {parent_session_id} seq {pause_seq}: {error}"
            ));
        }
    }

    /// Apply one native hold change. A change at or below the sequence already applied is ignored:
    /// replaying an old Resume must never reopen admission.
    pub fn apply(&self, parent_session_id: &str, event: &RunHoldChangeEvent) -> BridgeOutcome {
        let hold: EngineRunHold = read_engine_run_hold(&event.state);
        match event.change {
            RunHoldChange::Paused => {
                let _operation = self
                    .operation
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if !hold.stopped {
                    return BridgeOutcome::Ignored;
                }
                let mut applied = self
                    .last_applied
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if applied.is_some_and(|seq| seq >= hold.pause_seq) {
                    return BridgeOutcome::Ignored;
                }
                *applied = Some(hold.pause_seq);
                drop(applied);
                match self.engine.pause(parent_session_id, hold.pause_seq) {
                    Ok(()) => BridgeOutcome::Paused,
                    Err(error) => BridgeOutcome::Failed(error),
                }
            }
            RunHoldChange::Resumed => {
                if hold.stopped {
                    return BridgeOutcome::Ignored;
                }
                if self
                    .last_applied
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .is_some_and(|seq| seq > hold.pause_seq)
                {
                    return BridgeOutcome::StaleResume;
                }
                // The Stop's run fences are engine holds too, so the Resume event carries a seq
                // past the Stop it releases (#12249). Release the recorded Stop when stop <= event seq.
                let stop = self
                    .engine
                    .registry
                    .load(parent_session_id)
                    .ok()
                    .flatten()
                    .map(|record| record.pause_seq);
                let target_seq = match stop {
                    Some(stop) if stop <= hold.pause_seq => stop,
                    _ => hold.pause_seq,
                };
                let resume_id = format!("parent-resume:{target_seq}");
                Self::resume_seq(
                    &self.engine,
                    &self.operation,
                    &self.in_flight_resumes,
                    parent_session_id,
                    target_seq,
                    &resume_id,
                )
            }
        }
    }

    pub fn apply_arc(
        self: &Arc<Self>,
        parent_session_id: &str,
        event: &RunHoldChangeEvent,
    ) -> BridgeOutcome {
        let outcome = self.apply(parent_session_id, event);
        if matches!(event.change, RunHoldChange::Resumed) {
            let stop = self
                .engine
                .registry
                .load(parent_session_id)
                .ok()
                .flatten()
                .map(|record| record.pause_seq);
            let hold = read_engine_run_hold(&event.state);
            let target_seq = match stop {
                Some(stop) if stop <= hold.pause_seq => stop,
                _ => hold.pause_seq,
            };
            self.maybe_spawn_lost_recovery(parent_session_id, target_seq, &outcome);
        }
        outcome
    }

    /// Release a Stop left `applied` whose Resume the bridge missed (`#12342`/`#12343`).
    pub fn release_stale(
        self: &Arc<Self>,
        parent_session_id: &str,
        state: &EngineHoldState,
    ) -> BridgeOutcome {
        let Ok(Some(record)) = self.engine.registry.load(parent_session_id) else {
            return BridgeOutcome::Ignored;
        };
        if record.phase != PausePhase::Applied {
            return BridgeOutcome::Ignored;
        }
        if self
            .in_flight_resumes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(parent_session_id)
            == Some(&record.pause_seq)
        {
            return BridgeOutcome::Ignored;
        }
        let hold = read_engine_run_hold(state);
        if hold.pause_seq < record.pause_seq || hold.stopped {
            return BridgeOutcome::Ignored;
        }
        let resume_id = format!("parent-resume:{}", record.pause_seq);
        let outcome = Self::resume_seq(
            &self.engine,
            &self.operation,
            &self.in_flight_resumes,
            parent_session_id,
            record.pause_seq,
            &resume_id,
        );
        self.maybe_spawn_lost_recovery(parent_session_id, record.pause_seq, &outcome);
        outcome
    }

    /// Startup restoration: custody is rebuilt BEFORE the lifecycle reconcile reads the store.
    pub fn restore(&self, parent_session_id: &str) -> Option<u64> {
        self.engine
            .restore(parent_session_id)
            .map(|record| record.pause_seq)
    }

    /// The fenced runs of a restored Stop, so the reconcile knows which children are held.
    pub fn fenced_runs(&self, parent_session_id: &str) -> Vec<RunRef> {
        self.engine.refs(parent_session_id)
    }
}

/// How the composition reads a hold change off a host event. The Rust extension event carries no
/// hold projection of its own, so the host supplies the reader; `None` means "not a hold change".
pub type RunHoldChangeReader =
    Arc<dyn Fn(&maho_ext_api::ExtensionEvent) -> Option<RunHoldChangeEvent> + Send + Sync>;

/// `wireEventBridge`: register the hold-change handler on the host's event bus.
///
/// Custody is restored BEFORE the lifecycle reconcile runs (the restore edge), then the change is
/// applied, then the reconcile proceeds: an old Resume must never reopen admission before the
/// restored Stop is in place.
pub fn wire_run_pause_bridge(
    api: &mut ExtensionApi,
    bridge: Arc<EngineRunPauseBridge>,
    session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    read_hold_change: RunHoldChangeReader,
) {
    api.on(
        EventKind::Context,
        Arc::new(move |payload, _| {
            let bridge = bridge.clone();
            let session_id = session_id.clone();
            let read_hold_change = read_hold_change.clone();
            Box::pin(async move {
                let Some(event) = read_hold_change(payload) else {
                    return Ok(EventResult::None);
                };
                if let Some(session) = session_id() {
                    let _ = bridge.restore(&session);
                    let _ = bridge.apply(&session, &event);
                }
                Ok(EventResult::None)
            })
        }),
    );
}

/// The credential a resumed run presents; named here so the wiring and the bridge agree.
pub fn resume_credential(
    barrier: &senpi_task::pause::admission_barrier::AdmissionBarrier,
    parent: &str,
    pause_seq: u64,
    resume_id: &str,
) -> Result<ResumeCredential, senpi_task::pause::admission_barrier::AdmissionBarrierError> {
    barrier.resume(parent, pause_seq, resume_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_the_hold_change_event_with_a_legal_direction_is_recognised() {
        assert_eq!(
            is_run_hold_change(&json!({"type": "session_hold_changed", "change": "paused"})),
            Some(RunHoldChange::Paused)
        );
        assert_eq!(
            is_run_hold_change(&json!({"type": "session_hold_changed", "change": "resumed"})),
            Some(RunHoldChange::Resumed)
        );
        assert_eq!(
            is_run_hold_change(&json!({"type": "session_hold_changed", "change": "bogus"})),
            None
        );
        assert_eq!(
            is_run_hold_change(&json!({"type": "other", "change": "paused"})),
            None
        );
    }

    #[test]
    fn resume_releases_stop_pushed_past_by_run_fences_and_release_stale_releases_missed_stop() {
        use senpi_task::pause::{
            admission_barrier::AdmissionParticipant,
            ports::{EngineHold, EnginePauseState, RunPauseOwner},
            registry::PauseParticipant,
            run_controller::{
                Effect, PauseParticipantResult, ResumeParticipantRequest, ResumeParticipantResult,
                RunPauseEffects,
            },
        };
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct StubEffects {
            resumes: Arc<AtomicUsize>,
            ended: Arc<AtomicUsize>,
        }

        impl RunPauseEffects for StubEffects {
            fn snapshot(
                &self,
                _parent: &str,
                _admissions: &[AdmissionParticipant],
            ) -> Vec<PauseParticipant> {
                vec![PauseParticipant {
                    key: "t1".into(),
                    kind: senpi_task::pause::registry::ParticipantKind::Task,
                    run_ref: RunRef::task("t1"),
                    task_id: Some("t1".into()),
                    node_id: None,
                    run_epoch: Some(1),
                    admission: None,
                    state: senpi_task::pause::registry::ParticipantState::Pending,
                    ack_seq: None,
                    observed_child_seq: None,
                    resume_turn_id: None,
                    lost_reason: None,
                    at: "2026-01-01T00:00:00Z".into(),
                }]
            }
            fn quiesce<'a>(&'a self, _parent: &'a str, _seq: u64) -> Effect<'a, ()> {
                Box::pin(async { Ok(()) })
            }
            fn add_fences<'a>(
                &'a self,
                _parent: &'a str,
                _seq: u64,
                _runs: Vec<RunRef>,
            ) -> Effect<'a, ()> {
                Box::pin(async { Ok(()) })
            }
            fn pause<'a>(
                &'a self,
                _participant: &'a PauseParticipant,
                _owner: RunPauseOwner,
            ) -> Effect<'a, PauseParticipantResult> {
                Box::pin(async { Ok(PauseParticipantResult::Held { child_seq: 1 }) })
            }
            fn dispatch_guidance<'a>(
                &'a self,
                _parent: &'a str,
                _seq: u64,
                _resume: &'a str,
            ) -> Effect<'a, ()> {
                Box::pin(async { Ok(()) })
            }
            fn resume<'a>(
                &'a self,
                _participant: &'a PauseParticipant,
                _request: ResumeParticipantRequest,
                _credential: Option<senpi_task::pause::admission_barrier::ResumeCredential>,
            ) -> Effect<'a, ResumeParticipantResult> {
                self.resumes.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {
                    Ok(ResumeParticipantResult::Resumed {
                        turn_id: Some("turn-1".into()),
                    })
                })
            }
            fn install<'a>(
                &'a self,
                _parent: &'a str,
                _seq: u64,
                _credential: senpi_task::pause::admission_barrier::ResumeCredential,
            ) -> Effect<'a, ()> {
                Box::pin(async { Ok(()) })
            }
            fn acknowledge_installed<'a>(
                &'a self,
                _parent: &'a str,
                _seq: u64,
                _resume: &'a str,
            ) -> Effect<'a, ()> {
                Box::pin(async { Ok(()) })
            }
            fn cancel<'a>(
                &'a self,
                _participant: &'a PauseParticipant,
                _parent: &'a str,
            ) -> Effect<'a, ()> {
                Box::pin(async { Ok(()) })
            }
            fn end_lost<'a>(
                &'a self,
                _participant: &'a PauseParticipant,
                _reason: &'a str,
            ) -> Effect<'a, bool> {
                self.ended.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(true) })
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let resumes = Arc::new(AtomicUsize::new(0));
        let ended = Arc::new(AtomicUsize::new(0));
        let effects = Arc::new(StubEffects {
            resumes: resumes.clone(),
            ended,
        });
        let engine = Arc::new(EngineRunPause::new_with_effects(
            dir.path(),
            effects,
            Arc::default(),
            senpi_task::pause::admission_barrier::AdmissionBarrier::new(),
        ));
        let bridge = Arc::new(EngineRunPauseBridge::new(engine));

        // 1. Pause at seq 3, then run fences bump the host resume event seq to 6 (> 3).
        let paused = bridge.apply(
            "parent-1",
            &RunHoldChangeEvent {
                state: EngineHoldState::Native(EnginePauseState {
                    pause_seq: 3,
                    holds: vec![EngineHold {
                        kind: "session_hold".into(),
                        key: "parent".into(),
                        data: None,
                    }],
                }),
                change: RunHoldChange::Paused,
            },
        );
        assert_eq!(paused, BridgeOutcome::Paused);

        let resumed = bridge.apply(
            "parent-1",
            &RunHoldChangeEvent {
                state: EngineHoldState::Native(EnginePauseState {
                    pause_seq: 6,
                    holds: Vec::new(),
                }),
                change: RunHoldChange::Resumed,
            },
        );
        assert_eq!(
            resumed,
            BridgeOutcome::Resumed {
                installed: true,
                failures: 0,
                failed_keys: Vec::new(),
            }
        );
        assert_eq!(resumes.load(Ordering::SeqCst), 1);

        // 2. Missed Resume scenario (`release_stale`): pause at seq 8, live hold still stopped refuses,
        // older seq refuses, and seq >= 8 with no session_hold releases.
        let paused2 = bridge.apply(
            "parent-1",
            &RunHoldChangeEvent {
                state: EngineHoldState::Native(EnginePauseState {
                    pause_seq: 8,
                    holds: vec![EngineHold {
                        kind: "session_hold".into(),
                        key: "parent".into(),
                        data: None,
                    }],
                }),
                change: RunHoldChange::Paused,
            },
        );
        assert_eq!(paused2, BridgeOutcome::Paused);

        assert_eq!(
            bridge.release_stale(
                "parent-1",
                &EngineHoldState::Native(EnginePauseState {
                    pause_seq: 8,
                    holds: vec![EngineHold {
                        kind: "session_hold".into(),
                        key: "parent".into(),
                        data: None,
                    }],
                }),
            ),
            BridgeOutcome::Ignored
        );
        assert_eq!(
            bridge.release_stale(
                "parent-1",
                &EngineHoldState::Native(EnginePauseState {
                    pause_seq: 7,
                    holds: Vec::new(),
                }),
            ),
            BridgeOutcome::Ignored
        );
        assert_eq!(
            bridge.release_stale(
                "parent-1",
                &EngineHoldState::Native(EnginePauseState {
                    pause_seq: 8,
                    holds: Vec::new(),
                }),
            ),
            BridgeOutcome::Resumed {
                installed: true,
                failures: 0,
                failed_keys: Vec::new(),
            }
        );
        assert_eq!(resumes.load(Ordering::SeqCst), 2);
    }
}
