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
        Self::new_with_barrier(state_dir, effects, AdmissionBarrier::new())
    }

    /// Compose over a barrier the caller already owns (the task run pause shares it).
    pub fn new_with_barrier(state_dir: impl Into<std::path::PathBuf>, effects: Arc<EngineRunPauseEffects>, barrier: AdmissionBarrier) -> Self {
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

/// `createTaskRunPause`'s ports over the live manager and store: the task-record side of custody.
///
/// The Rust task record has no `pause`/`unpause` transition, so the pause is written straight onto
/// the record (`status: paused`, which the store encodes additively as `running` + `paused: true`),
/// exactly the shape `to_storable_task_record` expects.
pub struct ManagerRunPausePorts {
    pub manager: Arc<senpi_task::manager::TaskManager>,
    pub store: senpi_task::store::TaskRecordStore,
    pub barrier: AdmissionBarrier,
    /// The child engine's native pause controls. The Rust runners expose none yet, so this is `None`
    /// and a Stop records its participants as lost instead of pretending they were held.
    pub child: Option<Arc<dyn senpi_task::manager::run_pause::ChildPausePort>>,
}

impl ManagerRunPausePorts {
    fn now_ms(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

impl senpi_task::manager::run_pause::TaskRunPausePorts for ManagerRunPausePorts {
    fn store(&self) -> &senpi_task::store::TaskRecordStore {
        &self.store
    }

    fn now(&self) -> i64 {
        self.now_ms()
    }

    fn live_handle(&self, task_id: &str) -> Option<Arc<dyn senpi_task::manager::ManagedChildHandle>> {
        self.manager.get_resident_handle(task_id)
    }

    fn child(&self) -> Option<Arc<dyn senpi_task::manager::run_pause::ChildPausePort>> {
        self.child.clone()
    }

    fn barrier(&self) -> Option<AdmissionBarrier> {
        Some(self.barrier.clone())
    }

    fn remember_owner(&self, task_id: &str, owner: &RunPauseOwner) -> Result<(), senpi_task::store::StoreError> {
        let kind = match owner.run_ref.kind {
            senpi_task::pause::ports::RunRefKind::Dag => senpi_task::state::RunPauseKind::Dag,
            senpi_task::pause::ports::RunRefKind::Task => senpi_task::state::RunPauseKind::Task,
            senpi_task::pause::ports::RunRefKind::Workpool => senpi_task::state::RunPauseKind::Workpool,
            senpi_task::pause::ports::RunRefKind::Team => senpi_task::state::RunPauseKind::Team,
        };
        let recorded = senpi_task::state::RecordedRunPause {
            parent_session_id: owner.parent_session_id.clone(),
            parent_pause_seq: owner.parent_pause_seq,
            run_ref: senpi_task::state::RunPauseRef { kind, id: owner.run_ref.id.clone() },
        };
        self.store.mutate(task_id, move |fresh| {
            let mut next = fresh.clone();
            next.run_pause = Some(recorded.clone());
            next
        })?;
        Ok(())
    }

    fn forget_owner(&self, task_id: &str) -> Result<(), senpi_task::store::StoreError> {
        self.store.mutate(task_id, |fresh| {
            let mut next = fresh.clone();
            next.run_pause = None;
            next
        })?;
        Ok(())
    }

    fn apply_pause(&self, task_id: &str, timestamp: &str) -> bool {
        let applied = self.store.mutate(task_id, |fresh| {
            let mut next = fresh.clone();
            if next.status == senpi_task::state::TaskStatus::Running {
                next.status = senpi_task::state::TaskStatus::Paused;
                next.updated_at = timestamp.to_string();
            }
            next
        });
        matches!(applied, Ok(Some(record)) if record.status == senpi_task::state::TaskStatus::Paused)
    }

    fn apply_unpause(&self, task_id: &str, timestamp: &str) -> bool {
        let applied = self.store.mutate(task_id, |fresh| {
            let mut next = fresh.clone();
            if next.status == senpi_task::state::TaskStatus::Paused {
                next.status = senpi_task::state::TaskStatus::Running;
                next.updated_at = timestamp.to_string();
            }
            next
        });
        matches!(applied, Ok(Some(record)) if record.status == senpi_task::state::TaskStatus::Running)
    }

    fn restore(&self, _record: &senpi_task::state::TaskRecord) -> Option<Result<Arc<dyn senpi_task::manager::ManagedChildHandle>, String>> {
        // No Rust runner rebuilds a paused child without starting a turn, so there is no restore port.
        None
    }

    fn track(&self, _task_id: &str, _handle: Arc<dyn senpi_task::manager::ManagedChildHandle>, _model: &str, _epoch: i64, _turn: senpi_task::manager::run_pause::ResumedChildTurn) {
        // The resumed turn's outcome is already subscribed by the handle; nothing to re-track.
    }

    fn revive(&self, record: &senpi_task::state::TaskRecord, timestamp: String) -> senpi_task::state::TaskRecord {
        let mut revived = record.clone();
        revived.status = senpi_task::state::TaskStatus::Running;
        revived.updated_at = timestamp;
        revived.notification.run_epoch = record.notification.run_epoch.saturating_add(1);
        revived
    }
}

impl EngineRunPause {
    /// Compose the engine's pause controller over the live manager and store.
    pub fn compose(
        manager: Arc<senpi_task::manager::TaskManager>,
        store: senpi_task::store::TaskRecordStore,
        dag: Option<Arc<dyn RunPauseSource>>,
        state_dir: impl Into<std::path::PathBuf>,
    ) -> (Self, Arc<senpi_task::manager::run_pause::TaskRunPause>) {
        let barrier = AdmissionBarrier::new();
        let ports = Arc::new(ManagerRunPausePorts {
            manager: manager.clone(),
            store: store.clone(),
            barrier: barrier.clone(),
            child: None,
        });
        let run_pause = Arc::new(senpi_task::manager::run_pause::TaskRunPause::new(ports));
        let pause_task = {
            let run_pause = run_pause.clone();
            Arc::new(move |task_id: &str, owner: &RunPauseOwner| run_pause.pause_task(task_id, owner))
        };
        let resume_task = {
            let run_pause = run_pause.clone();
            Arc::new(move |task_id: &str, owner: &RunPauseOwner, resume_id: &str, cutoff: Option<u64>, credential: Option<&ResumeCredential>| {
                let result = run_pause.resume_paused_task(task_id, &senpi_task::manager::run_pause::ResumeTaskRequest {
                    owner: owner.clone(),
                    resume_credential: credential.cloned(),
                    resume_id: resume_id.to_string(),
                    child_seq_cutoff: cutoff.unwrap_or(0),
                })?;
                Ok(match result {
                    senpi_task::manager::run_pause::ResumeTaskResult::Resumed { turn_id } => ResumeEffect::Resumed { turn_id: Some(turn_id) },
                    senpi_task::manager::run_pause::ResumeTaskResult::OtherHoldLive { child_seq } => ResumeEffect::HeldByUser { child_seq: child_seq.unwrap_or(0) },
                })
            })
        };
        let observe = {
            let run_pause = run_pause.clone();
            Arc::new(move |task_id: &str, parent: &str| run_pause.observe_task_hold(task_id, parent))
        };
        let cancel_task = {
            let manager = manager.clone();
            Arc::new(move |task_id: &str| {
                manager
                    .cancel_task(task_id, Some("run_cancelled"), senpi_task::steering::CancelOptions::default())
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
        };
        let effects = Arc::new(EngineRunPauseEffects {
            sources: dag.into_iter().collect(),
            pause_task,
            resume_task,
            observe_task_hold: observe,
            hold_task_input: Arc::new(
                |_task_id: &str, _input: &senpi_task::manager::run_pause::HeldTaskInput| {
                    Err("no child pause port is installed".into())
                },
            ),
            fence_run: Arc::new(|_run: &RunRef| {}),
            release_fences: Arc::new(|_run: &RunRef| {}),
            cancel_task,
        });
        let engine = EngineRunPause::new_with_barrier(state_dir, effects, barrier);
        (engine, run_pause)
    }
}
