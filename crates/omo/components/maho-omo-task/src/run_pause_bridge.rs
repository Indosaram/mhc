//! `run-pause-bridge.ts`: bridge the host's native Stop/Resume onto persistent run custody.
//!
//! The host reports a hold change; this bridge validates it (a stale epoch is ignored, not applied),
//! serializes propagation and resume, restores custody BEFORE the lifecycle reconcile runs, and
//! retries the participants a failed dispatch left outside the run's custody.

use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::{EventKind, EventResult, ExtensionApi};
use senpi_task::pause::{admission_barrier::ResumeCredential, ports::RunRef};

use crate::engine_hold_reader::{EngineHoldState, EngineRunHold, read_engine_run_hold};
use crate::engine_run_pause::EngineRunPause;

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
}

impl EngineRunPauseBridge {
    pub fn new(engine: Arc<EngineRunPause>) -> Self {
        Self {
            engine,
            operation: Mutex::new(()),
            last_applied: Mutex::new(None),
        }
    }

    pub fn close_admission(&self, parent: &str, seq: u64) {
        self.engine.barrier.close(parent, seq);
    }

    /// Apply one native hold change. A change at or below the sequence already applied is ignored:
    /// replaying an old Resume must never reopen admission.
    pub fn apply(&self, parent_session_id: &str, event: &RunHoldChangeEvent) -> BridgeOutcome {
        let hold: EngineRunHold = read_engine_run_hold(&event.state);
        let _operation = self
            .operation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match event.change {
            RunHoldChange::Paused => {
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
                let Some(request) = resume_request(&hold) else {
                    return BridgeOutcome::Ignored;
                };
                if self
                    .last_applied
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .is_some_and(|seq| seq > request.pause_seq)
                {
                    return BridgeOutcome::StaleResume;
                }
                match self
                    .engine
                    .resume(parent_session_id, request.pause_seq, &request.resume_id)
                {
                    Ok(senpi_task::pause::run_controller::RunResumeResult::Installed {
                        failures,
                    }) => {
                        let released = self.engine.release(
                            parent_session_id,
                            request.pause_seq,
                            &request.resume_id,
                        );
                        // A Resume that installed but never released admission is retried as lost.
                        let failures = if released {
                            failures.len()
                        } else {
                            failures.len()
                                + self
                                    .engine
                                    .retry_lost(parent_session_id, request.pause_seq)
                                    .len()
                        };
                        BridgeOutcome::Resumed {
                            installed: released,
                            failures,
                        }
                    }
                    Ok(senpi_task::pause::run_controller::RunResumeResult::Superseded) => {
                        BridgeOutcome::StaleResume
                    }
                    Err(error) => BridgeOutcome::Failed(error),
                }
            }
        }
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

fn resume_request(hold: &EngineRunHold) -> Option<ResumeRequest> {
    if hold.stopped {
        return None;
    }
    Some(ResumeRequest {
        pause_seq: hold.pause_seq,
        resume_id: format!("parent-resume:{}", hold.pause_seq),
    })
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
pub fn resume_credential(barrier: &senpi_task::pause::admission_barrier::AdmissionBarrier, parent: &str, pause_seq: u64, resume_id: &str) -> Result<ResumeCredential, senpi_task::pause::admission_barrier::AdmissionBarrierError> {
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
}
