//! `engine-run-pause.ts`: persistent parent-run pause ownership over the owned child-session subtree.
//!
//! A Stop on the parent session must reach EVERY run this session owns - DAG runs, tasks, workpool
//! workers, team members - hold each child non-terminally, and keep the admission barrier closed so
//! no new child slips in behind it. A Resume reopens admission under a credential, releases each held
//! child, and installs the resumed runs before admission is released. A cancelled run leaves custody
//! and releases its fences.

use std::sync::Arc;

use senpi_task::pause::{
    admission_barrier::{AdmissionBarrier, AdmissionParticipant, ResumeCredential},
    ports::{RunPauseOwner, RunRef},
    registry::{PauseParticipant, PauseParticipantKind, PauseParticipantState, PauseRegistry, PauseRegistryRecord, ParticipantPatch},
    run_controller::{ResumeEffect, RunPauseController, RunPauseEffects, RunResumeResult},
};

/// What a pause source contributes to one parent's custody.
pub trait RunPauseSource: Send + Sync {
    /// The runs this source owns for the parent, as references.
    fn refs(&self, parent_session_id: &str) -> Vec<RunRef>;
    /// Retire scheduling before the durable user_stop lands.
    fn quiesce(&self, _parent_session_id: &str, _pause_seq: u64) {}
    /// Dispatch the final guidance revisions into the held input custody.
    fn dispatch_guidance(&self, _parent_session_id: &str, _pause_seq: u64, _resume_id: &str) {}
    /// Install the resumed runs under the admission credential.
    fn install(&self, _parent_session_id: &str, _pause_seq: u64, _credential: &ResumeCredential) -> Result<(), String> {
        Ok(())
    }
    /// Cancel one run of this source.
    fn cancel(&self, _parent_session_id: &str, _run_ref: &RunRef) -> Result<(), String> {
        Ok(())
    }
    /// The participants this source contributes to the parent's Stop.
    fn snapshot(&self, _parent_session_id: &str) -> Vec<PauseParticipant> {
        Vec::new()
    }
}

/// The effects the engine hands the controller: every source plus the task manager's own ports.
pub struct EngineRunPauseEffects {
    pub sources: Vec<Arc<dyn RunPauseSource>>,
    pub pause_task: Arc<dyn Fn(&str, &RunPauseOwner) -> Result<Option<senpi_task::manager::run_pause::EngineInheritReceipt>, String> + Send + Sync>,
    pub resume_task: Arc<dyn Fn(&str, &RunPauseOwner, &str, Option<u64>, Option<&ResumeCredential>) -> Result<ResumeEffect, String> + Send + Sync>,
    pub observe_task_hold: Arc<dyn Fn(&str, &str) -> Option<senpi_task::manager::run_pause::ObservedHold> + Send + Sync>,
    pub hold_task_input: Arc<dyn Fn(&str, &senpi_task::manager::run_pause::HeldTaskInput) -> Result<senpi_task::manager::run_pause::EngineHoldInputResult, String> + Send + Sync>,
    pub fence_run: Arc<dyn Fn(&RunRef) + Send + Sync>,
    pub release_fences: Arc<dyn Fn(&RunRef) + Send + Sync>,
    pub cancel_task: Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>,
}

impl RunPauseEffects for EngineRunPauseEffects {
    fn snapshot(&self, parent_session_id: &str, admissions: &[AdmissionParticipant]) -> Vec<PauseParticipant> {
        let mut participants: Vec<PauseParticipant> = self.sources.iter().flat_map(|source| source.snapshot(parent_session_id)).collect();
        for admission in admissions {
            participants.push(PauseParticipant {
                key: admission.key.clone(),
                kind: PauseParticipantKind::HeldAdmission,
                run_ref: admission.run_ref.clone(),
                task_id: None,
                node_id: None,
                run_epoch: None,
                admission: None,
                state: PauseParticipantState::Pending,
                ack_seq: None,
                observed_child_seq: None,
                resume_turn_id: None,
                lost_reason: None,
                at: String::new(),
            });
        }
        participants
    }

    fn quiesce(&self, parent_session_id: &str, pause_seq: u64) {
        for source in &self.sources {
            source.quiesce(parent_session_id, pause_seq);
        }
    }

    fn add_fences(&self, _parent_session_id: &str, _pause_seq: u64, runs: &[RunRef]) {
        for run in runs {
            (self.fence_run)(run);
        }
    }

    fn pause(&self, participant: &PauseParticipant, owner: &RunPauseOwner) -> Result<senpi_task::pause::run_controller::PauseEffect, String> {
        let Some(task_id) = &participant.task_id else {
            return Ok(senpi_task::pause::run_controller::PauseEffect::Settled);
        };
        match (self.pause_task)(task_id, owner)? {
            Some(receipt) => Ok(senpi_task::pause::run_controller::PauseEffect::Held { child_seq: receipt.child_seq }),
            None => Ok(senpi_task::pause::run_controller::PauseEffect::Settled),
        }
    }

    fn dispatch_guidance(&self, parent_session_id: &str, pause_seq: u64, resume_id: &str) {
        for source in &self.sources {
            source.dispatch_guidance(parent_session_id, pause_seq, resume_id);
        }
    }

    fn resume(
        &self,
        participant: &PauseParticipant,
        request: &senpi_task::pause::run_controller::ResumeParticipantRequest,
        credential: Option<&ResumeCredential>,
    ) -> Result<ResumeEffect, String> {
        let Some(task_id) = &participant.task_id else {
            return Ok(ResumeEffect::Settled);
        };
        (self.resume_task)(task_id, &request.owner, &request.resume_id, request.child_seq_cutoff, credential)
    }

    fn install(&self, parent_session_id: &str, pause_seq: u64, credential: &ResumeCredential) -> Result<(), String> {
        for source in &self.sources {
            source.install(parent_session_id, pause_seq, credential)?;
        }
        Ok(())
    }

    fn acknowledge_installed(&self, _parent_session_id: &str, _pause_seq: u64, _resume_id: &str) {}

    fn cancel(&self, participant: &PauseParticipant, parent_session_id: &str) -> Result<(), String> {
        if let Some(source) = self.sources.iter().find(|source| source.refs(parent_session_id).iter().any(|run| run == &participant.run_ref)) {
            source.cancel(parent_session_id, &participant.run_ref)?;
        }
        if let Some(task_id) = &participant.task_id {
            (self.cancel_task)(task_id)?;
        }
        (self.release_fences)(&participant.run_ref);
        Ok(())
    }

    fn observe_hold(&self, participant: &PauseParticipant, parent_session_id: &str) -> Option<senpi_task::manager::run_pause::ObservedHold> {
        (self.observe_task_hold)(participant.task_id.as_deref()?, parent_session_id)
    }
}

/// The engine's pause controller: the registry, the controller, and the barrier they share.
pub struct EngineRunPause {
    pub registry: Arc<PauseRegistry>,
    pub controller: Arc<RunPauseController>,
    pub barrier: AdmissionBarrier,
    sources: Vec<Arc<dyn RunPauseSource>>,
}

impl EngineRunPause {
    pub fn new(state_dir: impl Into<std::path::PathBuf>, effects: Arc<EngineRunPauseEffects>) -> Self {
        let barrier = AdmissionBarrier::new();
        let registry = Arc::new(PauseRegistry::new(state_dir, barrier.clone()));
        let sources = effects.sources.clone();
        let controller = Arc::new(RunPauseController::new(Arc::clone(&registry), effects));
        Self { registry, controller, barrier, sources }
    }

    /// `refs`: every run this session owns, across every source.
    pub fn refs(&self, parent_session_id: &str) -> Vec<RunRef> {
        let mut refs: Vec<RunRef> = Vec::new();
        for source in &self.sources {
            for run in source.refs(parent_session_id) {
                if !refs.contains(&run) {
                    refs.push(run);
                }
            }
        }
        refs
    }

    /// `ownedSubtree`: the parent plus every run reference it owns, deduplicated.
    pub fn owned_subtree(&self, parent_session_id: &str) -> Vec<RunRef> {
        self.refs(parent_session_id)
    }

    /// The durable Stop. A failed propagation still leaves a resumable record.
    pub fn pause(&self, parent_session_id: &str, pause_seq: u64) -> Result<(), String> {
        match self.controller.pause(parent_session_id, pause_seq) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = self.controller.record_propagation_failure(parent_session_id, pause_seq, &error.to_string());
                Err(error.to_string())
            }
        }
    }

    /// The Resume, then the admission release.
    pub fn resume(&self, parent_session_id: &str, pause_seq: u64, resume_id: &str) -> Result<RunResumeResult, String> {
        self.controller.resume(parent_session_id, pause_seq, resume_id).map_err(|error| error.to_string())
    }

    /// A repeated Resume retries only the participants still outside custody.
    pub fn retry_lost(&self, parent_session_id: &str, pause_seq: u64) -> Vec<senpi_task::pause::run_controller::ResumeFailure> {
        self.controller.retry_lost(parent_session_id, pause_seq).unwrap_or_default()
    }

    pub fn release(&self, parent_session_id: &str, pause_seq: u64, resume_id: &str) -> bool {
        self.controller.released(parent_session_id, pause_seq, resume_id)
    }

    pub fn cancel(&self, parent_session_id: &str, run_ref: &RunRef) -> Result<(), String> {
        self.controller.cancel(parent_session_id, run_ref).map_err(|error| error.to_string())
    }

    /// The record a restarted host restores before its reconcile.
    pub fn restore(&self, parent_session_id: &str) -> Option<PauseRegistryRecord> {
        self.registry.load(parent_session_id).ok().flatten()
    }

    /// The current admission state of one root.
    pub fn admission_state(&self, parent_session_id: &str) -> senpi_task::pause::admission_barrier::AdmissionState {
        self.barrier.state(parent_session_id)
    }

    /// Mark one participant held after an observed child hold (the bridge's reconcile path).
    pub fn record_observed_hold(&self, parent_session_id: &str, pause_seq: u64, key: &str, child_seq: u64) -> bool {
        self.registry.update(parent_session_id, pause_seq, key, &ParticipantPatch { state: Some(PauseParticipantState::Held), observed_child_seq: Some(Some(child_seq)), ..ParticipantPatch::default() }).unwrap_or(false)
    }
}
