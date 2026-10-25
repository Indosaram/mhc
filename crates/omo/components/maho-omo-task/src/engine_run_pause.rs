//! `engine-run-pause.ts`: persistent parent-run pause ownership over the owned child-session subtree.
//!
//! A Stop on the parent session must reach EVERY run this session owns - DAG runs, tasks, workpool
//! workers, team members - hold each child non-terminally, and keep the admission barrier closed so
//! no new child slips in behind it. A Resume reopens admission under a credential, releases each held
//! child, and installs the resumed runs before admission is released. A cancelled run leaves custody
//! and releases its fences.

pub type PauseTaskCallback = Arc<
        dyn Fn(
                &str,
                &RunPauseOwner,
            )
                -> Result<Option<senpi_task::manager::run_pause::EngineInheritReceipt>, String>
            + Send
            + Sync,
    >;
pub type ResumeTaskCallback = Arc<
        dyn Fn(
                &str,
                &RunPauseOwner,
                &str,
                Option<u64>,
                Option<&ResumeCredential>,
            ) -> Result<ResumeParticipantResult, String>
            + Send
            + Sync,
    >;
pub type ObserveTaskHoldCallback = Arc<
        dyn Fn(&str, &str) -> Option<senpi_task::manager::run_pause::ObservedHold> + Send + Sync,
    >;
pub type HoldTaskInputCallback = Arc<
        dyn Fn(
                &str,
                &senpi_task::manager::run_pause::HeldTaskInput,
            ) -> Result<senpi_task::manager::run_pause::EngineHoldInputResult, String>
            + Send
            + Sync,
    >;
pub type FenceRunCallback = Arc<dyn Fn(&RunRef) -> Result<(), String> + Send + Sync>;
pub type CancelTaskCallback = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
use senpi_task::pause::registry::PauseError;
use std::sync::{Arc, Mutex, PoisonError};

use senpi_task::pause::{
    admission_barrier::{AdmissionBarrier, AdmissionParticipant, ResumeCredential},
    ports::{RunPauseOwner, RunRef},
    registry::{
        ParticipantKind, ParticipantState, PauseParticipant, PauseRegistry, PauseRegistryRecord,
    },
    run_controller::{
        Effect, PauseParticipantResult, ResumeParticipantResult, RunPauseController,
        RunPauseEffects, RunResumeResult,
    },
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
    fn install(
        &self,
        _parent_session_id: &str,
        _pause_seq: u64,
        _credential: &ResumeCredential,
    ) -> Result<(), String> {
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
    fn released(&self, _parent: &str) -> Result<(), String> {
        Ok(())
    }
    fn run_ref_for_task(&self, _task: &str) -> Option<RunRef> {
        None
    }
}

/// The effects the engine hands the controller: every source plus the task manager's own ports.
pub struct EngineRunPauseEffects {
    pub sources: Arc<Mutex<Vec<Arc<dyn RunPauseSource>>>>,
    pub task_records: Arc<dyn Fn() -> Vec<senpi_task::state::TaskRecord> + Send + Sync>,
    pub pause_task: PauseTaskCallback,
    pub resume_task: ResumeTaskCallback,
    pub observe_task_hold: ObserveTaskHoldCallback,
    pub hold_task_input: HoldTaskInputCallback,
    pub fence_run: FenceRunCallback,
    pub release_fences: FenceRunCallback,
    pub cancel_task: CancelTaskCallback,
}

impl RunPauseEffects for EngineRunPauseEffects {
    fn snapshot(&self, parent: &str, admissions: &[AdmissionParticipant]) -> Vec<PauseParticipant> {
        let sources = self
            .sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let records = (self.task_records)();
        let mut sessions = vec![parent.to_string()];
        loop {
            let before = sessions.len();
            for record in &records {
                if sessions.contains(&record.parent_session_id)
                    && let Some(child) = &record.child_session_id
                        && !sessions.contains(child) {
                            sessions.push(child.clone());
                        }
            }
            if before == sessions.len() {
                break;
            }
        }
        let mut participants: Vec<_> = records
            .into_iter()
            .filter(|record| {
                sessions.contains(&record.parent_session_id)
                    && matches!(
                        record.status,
                        senpi_task::state::TaskStatus::Running
                            | senpi_task::state::TaskStatus::Pending
                            | senpi_task::state::TaskStatus::Paused
                    )
            })
            .map(|record| {
                let run_ref = sources
                    .iter()
                    .find_map(|source| source.run_ref_for_task(&record.task_id))
                    .unwrap_or_else(|| {
                        if let Some(owner) = &record.owner {
                            RunRef::new(
                                senpi_task::pause::ports::RunRefKind::Dag,
                                owner.run_id.clone(),
                            )
                        } else if let Some(team) = &record.team_run_id {
                            RunRef::new(senpi_task::pause::ports::RunRefKind::Team, team.clone())
                        } else {
                            RunRef::task(&record.task_id)
                        }
                    });
                PauseParticipant {
                    key: record.task_id.clone(),
                    kind: if record.status == senpi_task::state::TaskStatus::Pending {
                        ParticipantKind::HeldAdmission
                    } else {
                        match run_ref.kind {
                            senpi_task::pause::ports::RunRefKind::Dag => ParticipantKind::DagNode,
                            senpi_task::pause::ports::RunRefKind::Workpool => {
                                ParticipantKind::WorkpoolWorker
                            }
                            senpi_task::pause::ports::RunRefKind::Team => {
                                ParticipantKind::TeamMember
                            }
                            senpi_task::pause::ports::RunRefKind::Task => ParticipantKind::Task,
                        }
                    },
                    run_ref,
                    task_id: Some(record.task_id),
                    node_id: record.owner.map(|owner| owner.node_id),
                    run_epoch: u64::try_from(record.notification.run_epoch).ok(),
                    admission: None,
                    state: ParticipantState::Pending,
                    ack_seq: None,
                    observed_child_seq: None,
                    resume_turn_id: None,
                    lost_reason: None,
                    at: record.updated_at,
                }
            })
            .collect();
        participants.extend(sources.iter().flat_map(|source| source.snapshot(parent)));
        for source in &sources {
            for run_ref in source.refs(parent) {
                let key = format!(
                    "run:{}:{}",
                    match run_ref.kind {
                        senpi_task::pause::ports::RunRefKind::Dag => "dag",
                        senpi_task::pause::ports::RunRefKind::Workpool => "workpool",
                        senpi_task::pause::ports::RunRefKind::Team => "team",
                        senpi_task::pause::ports::RunRefKind::Task => "task",
                    },
                    run_ref.id
                );
                if !participants
                    .iter()
                    .any(|participant| participant.key == key)
                {
                    participants.push(PauseParticipant {
                        key,
                        kind: ParticipantKind::HeldAdmission,
                        run_ref,
                        task_id: None,
                        node_id: None,
                        run_epoch: None,
                        admission: None,
                        state: ParticipantState::Pending,
                        ack_seq: None,
                        observed_child_seq: None,
                        resume_turn_id: None,
                        lost_reason: None,
                        at: String::new(),
                    });
                }
            }
        }
        let known = participants
            .iter()
            .map(|participant| participant.key.clone())
            .collect::<Vec<_>>();
        participants.extend(
            admissions
                .iter()
                .filter(|admission| !known.contains(&admission.key))
                .map(|admission| PauseParticipant {
                    key: admission.key.clone(),
                    kind: ParticipantKind::HeldAdmission,
                    run_ref: admission.run_ref.clone(),
                    task_id: None,
                    node_id: None,
                    run_epoch: None,
                    admission: None,
                    state: ParticipantState::Pending,
                    ack_seq: None,
                    observed_child_seq: None,
                    resume_turn_id: None,
                    lost_reason: None,
                    at: String::new(),
                }),
        );
        participants
    }
    fn quiesce<'a>(&'a self, parent: &'a str, seq: u64) -> Effect<'a, ()> {
        Box::pin(async move {
            for source in self
                .sources
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
            {
                source.quiesce(parent, seq);
            }
            Ok(())
        })
    }
    fn add_fences<'a>(&'a self, _parent: &'a str, _seq: u64, runs: Vec<RunRef>) -> Effect<'a, ()> {
        Box::pin(async move {
            for run in runs {
                (self.fence_run)(&run).map_err(PauseError::message)?;
            }
            Ok(())
        })
    }
    fn pause<'a>(
        &'a self,
        participant: &'a PauseParticipant,
        owner: RunPauseOwner,
    ) -> Effect<'a, PauseParticipantResult> {
        Box::pin(async move {
            let Some(task) = &participant.task_id else {
                return Ok(PauseParticipantResult::Settled);
            };
            Ok(
                match (self.pause_task)(task, &owner).map_err(PauseError::message)? {
                    Some(receipt) => PauseParticipantResult::Held {
                        child_seq: receipt.child_seq,
                    },
                    None => PauseParticipantResult::Settled,
                },
            )
        })
    }
    fn dispatch_guidance<'a>(
        &'a self,
        parent: &'a str,
        seq: u64,
        resume: &'a str,
    ) -> Effect<'a, ()> {
        Box::pin(async move {
            for source in self
                .sources
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
            {
                source.dispatch_guidance(parent, seq, resume);
            }
            Ok(())
        })
    }
    fn resume<'a>(
        &'a self,
        participant: &'a PauseParticipant,
        request: senpi_task::pause::run_controller::ResumeParticipantRequest,
        credential: Option<ResumeCredential>,
    ) -> Effect<'a, ResumeParticipantResult> {
        Box::pin(async move {
            let Some(task) = &participant.task_id else {
                return Ok(ResumeParticipantResult::Settled);
            };
            (self.resume_task)(
                task,
                &request.owner,
                &request.resume_id,
                request.child_seq_cutoff,
                credential.as_ref(),
            )
            .map_err(PauseError::message)
        })
    }
    fn install<'a>(
        &'a self,
        parent: &'a str,
        seq: u64,
        credential: ResumeCredential,
    ) -> Effect<'a, ()> {
        Box::pin(async move {
            for source in self
                .sources
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
            {
                source
                    .install(parent, seq, &credential)
                    .map_err(PauseError::message)?;
            }
            Ok(())
        })
    }
    fn acknowledge_installed<'a>(
        &'a self,
        _parent: &'a str,
        _seq: u64,
        _resume: &'a str,
    ) -> Effect<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn cancel<'a>(&'a self, participant: &'a PauseParticipant, parent: &'a str) -> Effect<'a, ()> {
        Box::pin(async move {
            for source in self
                .sources
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
            {
                if source.refs(parent).contains(&participant.run_ref) {
                    source
                        .cancel(parent, &participant.run_ref)
                        .map_err(PauseError::message)?;
                }
            }
            if let Some(task) = &participant.task_id {
                (self.cancel_task)(task).map_err(PauseError::message)?;
            }
            (self.release_fences)(&participant.run_ref).map_err(PauseError::message)?;
            Ok(())
        })
    }
    fn observe_hold<'a>(
        &'a self,
        participant: &'a PauseParticipant,
        parent: &'a str,
    ) -> Effect<'a, Option<senpi_task::pause::ports::ObservedHold>> {
        Box::pin(async move {
            Ok(participant
                .task_id
                .as_deref()
                .and_then(|task| (self.observe_task_hold)(task, parent))
                .map(|hold| senpi_task::pause::ports::ObservedHold {
                    held: hold.held,
                    child_seq: hold.child_seq,
                }))
        })
    }
}

/// The engine's pause controller: the registry, the controller, and the barrier they share.
pub struct EngineRunPause {
    pub registry: Arc<PauseRegistry>,
    pub controller: Arc<RunPauseController>,
    pub barrier: AdmissionBarrier,
    sources: Arc<Mutex<Vec<Arc<dyn RunPauseSource>>>>,
}

impl EngineRunPause {
    pub fn new(
        state_dir: impl Into<std::path::PathBuf>,
        effects: Arc<EngineRunPauseEffects>,
    ) -> Self {
        Self::new_with_barrier(state_dir, effects, AdmissionBarrier::new())
    }

    /// Compose over a barrier the caller already owns (the task run pause shares it).
    pub fn new_with_barrier(
        state_dir: impl Into<std::path::PathBuf>,
        effects: Arc<EngineRunPauseEffects>,
        barrier: AdmissionBarrier,
    ) -> Self {
        let registry = Arc::new(PauseRegistry::new(state_dir, barrier.clone()));
        let sources = effects.sources.clone();
        let controller = Arc::new(RunPauseController::new((*registry).clone(), effects));
        Self {
            registry,
            controller,
            barrier,
            sources,
        }
    }

    pub fn register(&self, source: Arc<dyn RunPauseSource>) {
        self.sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(source);
    }

    /// `refs`: every run this session owns, across every source.
    pub fn refs(&self, parent_session_id: &str) -> Vec<RunRef> {
        let mut refs: Vec<RunRef> = Vec::new();
        for source in self
            .sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
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
        match futures::executor::block_on(self.controller.pause(parent_session_id, pause_seq)) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = self.controller.record_propagation_failure(
                    parent_session_id,
                    pause_seq,
                    &error.to_string(),
                );
                Err(error.to_string())
            }
        }
    }

    /// The Resume, then the admission release.
    pub fn resume(
        &self,
        parent_session_id: &str,
        pause_seq: u64,
        resume_id: &str,
    ) -> Result<RunResumeResult, String> {
        futures::executor::block_on(
            self.controller
                .resume(parent_session_id, pause_seq, resume_id),
        )
        .map_err(|error| error.to_string())
    }

    /// A repeated Resume retries only the participants still outside custody.
    pub fn retry_lost(
        &self,
        parent_session_id: &str,
        pause_seq: u64,
    ) -> Vec<senpi_task::pause::run_controller::ResumeFailure> {
        match futures::executor::block_on(self.controller.retry_lost(parent_session_id, pause_seq))
        {
            Ok(failures) => failures,
            Err(error) => {
                maho_ext_api::diagnostic(&format!("pause retry failed: {error}"));
                vec![senpi_task::pause::run_controller::ResumeFailure {
                    key: parent_session_id.into(),
                    reason: error.to_string(),
                }]
            }
        }
    }

    pub fn release(&self, parent_session_id: &str, pause_seq: u64, resume_id: &str) -> bool {
        match self
            .controller
            .released(parent_session_id, pause_seq, resume_id)
        {
            Ok(true) => {
                for source in self
                    .sources
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
                {
                    if let Err(error) = source.released(parent_session_id) {
                        maho_ext_api::diagnostic(&format!("pause source release failed: {error}"));
                        return false;
                    }
                }
                true
            }
            Ok(false) => false,
            Err(error) => {
                maho_ext_api::diagnostic(&format!("pause release failed: {error}"));
                false
            }
        }
    }

    pub fn cancel(&self, parent_session_id: &str, run_ref: &RunRef) -> Result<(), String> {
        futures::executor::block_on(self.controller.cancel(parent_session_id, run_ref))
            .map_err(|error| error.to_string())
    }

    /// The record a restarted host restores before its reconcile.
    pub fn restore(&self, parent_session_id: &str) -> Option<PauseRegistryRecord> {
        self.registry.load(parent_session_id).ok().flatten()
    }

    /// The current admission state of one root.
    pub fn admission_state(
        &self,
        parent_session_id: &str,
    ) -> senpi_task::pause::admission_barrier::AdmissionState {
        self.barrier.state(parent_session_id)
    }

    /// Mark one participant held after an observed child hold (the bridge's reconcile path).
    pub fn record_observed_hold(
        &self,
        parent_session_id: &str,
        pause_seq: u64,
        key: &str,
        child_seq: u64,
    ) -> bool {
        self.registry
            .update(parent_session_id, pause_seq, key, |participant| {
                participant.state = ParticipantState::Held;
                participant.observed_child_seq = Some(child_seq);
            })
            .unwrap_or_else(|error| {
                maho_ext_api::diagnostic(&format!("pause observation failed: {error}"));
                false
            })
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
    pub child: Option<Arc<dyn senpi_task::manager::run_pause::TaskChildPausePort>>,
}

impl ManagerRunPausePorts {
    fn now_ms(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

impl senpi_task::manager::run_pause::TaskRunPausePorts for ManagerRunPausePorts {
    fn acquire_resume_capacity(
        &self,
        task_id: &str,
        epoch: i64,
        allocation: Option<&senpi_task::manager::concurrency::QueuedTaskAdmission>,
    ) -> Result<(), String> {
        self.manager
            .acquire_resume_capacity(task_id, epoch, allocation)
    }
    fn park_capacity(&self, task_id: &str, epoch: i64) {
        self.manager.park_run_capacity(task_id, epoch);
    }

    fn begin_native_pause(&self, task_id: &str, epoch: i64) {
        self.manager.begin_native_pause(task_id, epoch);
    }
    fn finish_native_pause(&self, task_id: &str, epoch: i64, accepted: bool) -> Result<(), String> {
        self.manager.finish_native_pause(task_id, epoch, accepted)
    }

    fn store(&self) -> &senpi_task::store::TaskRecordStore {
        &self.store
    }

    fn now(&self) -> i64 {
        self.now_ms()
    }

    fn live_handle(
        &self,
        task_id: &str,
    ) -> Option<Arc<dyn senpi_task::manager::ManagedChildHandle>> {
        self.manager.get_resident_handle(task_id)
    }

    fn child(&self) -> Option<Arc<dyn senpi_task::manager::run_pause::TaskChildPausePort>> {
        self.child.clone()
    }

    fn barrier(&self) -> Option<AdmissionBarrier> {
        Some(self.barrier.clone())
    }

    fn remember_owner(
        &self,
        task_id: &str,
        owner: &RunPauseOwner,
    ) -> Result<(), senpi_task::store::StoreError> {
        let kind = match owner.run_ref.kind {
            senpi_task::pause::ports::RunRefKind::Dag => senpi_task::state::RunPauseKind::Dag,
            senpi_task::pause::ports::RunRefKind::Task => senpi_task::state::RunPauseKind::Task,
            senpi_task::pause::ports::RunRefKind::Workpool => {
                senpi_task::state::RunPauseKind::Workpool
            }
            senpi_task::pause::ports::RunRefKind::Team => senpi_task::state::RunPauseKind::Team,
        };
        let recorded = senpi_task::state::RecordedRunPause {
            parent_session_id: owner.parent_session_id.clone(),
            parent_pause_seq: owner.parent_pause_seq,
            run_ref: senpi_task::state::RunPauseRef {
                kind,
                id: owner.run_ref.id.clone(),
            },
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

    fn restore(
        &self,
        record: &senpi_task::state::TaskRecord,
    ) -> Option<Result<Arc<dyn senpi_task::manager::ManagedChildHandle>, String>> {
        Some(self.manager.restore_paused_task(&record.task_id))
    }

    fn track(
        &self,
        task_id: &str,
        handle: Arc<dyn senpi_task::manager::ManagedChildHandle>,
        model: &str,
        epoch: i64,
        turn: senpi_task::manager::run_pause::TaskResumedTurn,
    ) {
        self.manager
            .track_resumed_turn(task_id, handle, model, epoch, turn.outcome);
    }

    fn revive(
        &self,
        record: &senpi_task::state::TaskRecord,
        timestamp: String,
    ) -> senpi_task::state::TaskRecord {
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
        runtime: Arc<Mutex<crate::runtime_context::TaskRuntimeContext>>,
    ) -> (Self, Arc<senpi_task::manager::run_pause::TaskRunPause>) {
        let barrier = manager.admission_barrier().clone();
        let ports = Arc::new(ManagerRunPausePorts {
            manager: manager.clone(),
            store: store.clone(),
            barrier: barrier.clone(),
            child: Some(Arc::new(NativeTaskPausePort)),
        });
        let run_pause = Arc::new(senpi_task::manager::run_pause::TaskRunPause::new(ports));
        let pause_task = {
            let run_pause = run_pause.clone();
            Arc::new(move |task_id: &str, owner: &RunPauseOwner| {
                run_pause.pause_task(task_id, owner)
            })
        };
        let resume_task = {
            let run_pause = run_pause.clone();
            Arc::new(
                move |task_id: &str,
                      owner: &RunPauseOwner,
                      resume_id: &str,
                      cutoff: Option<u64>,
                      credential: Option<&ResumeCredential>| {
                    let result = run_pause.resume_paused_task(
                        task_id,
                        &senpi_task::manager::run_pause::ResumeTaskRequest {
                            allocation: None,
                            owner: owner.clone(),
                            resume_credential: credential.cloned(),
                            resume_id: resume_id.to_string(),
                            child_seq_cutoff: cutoff.unwrap_or(0),
                        },
                    )?;
                    Ok(match result {
                        senpi_task::manager::run_pause::ResumeTaskResult::Resumed { turn_id } => {
                            ResumeParticipantResult::Resumed {
                                turn_id: Some(turn_id),
                            }
                        }
                        senpi_task::manager::run_pause::ResumeTaskResult::OtherHoldLive {
                            child_seq,
                        } => ResumeParticipantResult::HeldByUser {
                            child_seq: child_seq.unwrap_or(0),
                        },
                    })
                },
            )
        };
        let observe = {
            let run_pause = run_pause.clone();
            Arc::new(move |task_id: &str, parent: &str| {
                run_pause.observe_task_hold(task_id, parent)
            })
        };
        let cancel_task = {
            let manager = manager.clone();
            Arc::new(move |task_id: &str| {
                manager
                    .cancel_task(
                        task_id,
                        Some("run_cancelled"),
                        senpi_task::steering::CancelOptions::default(),
                    )
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
        };
        let effects = Arc::new(EngineRunPauseEffects {
            sources: Arc::new(Mutex::new(dag.into_iter().collect())),
            task_records: {
                let manager = manager.clone();
                Arc::new(move || {
                    manager
                        .list(&senpi_task::manager::types::ListScope::All)
                        .into_iter()
                        .map(|entry| entry.record)
                        .collect()
                })
            },
            pause_task,
            resume_task,
            observe_task_hold: observe,
            hold_task_input: Arc::new(
                |_task_id: &str, _input: &senpi_task::manager::run_pause::HeldTaskInput| {
                    Err("no child pause port is installed".into())
                },
            ),
            fence_run: {
                let runtime = runtime.clone();
                Arc::new(move |run| {
                    runtime
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .fence_run(serde_json::to_value(run).map_err(|error| error.to_string())?)
                        .map_err(|error| error.to_string())
                })
            },
            release_fences: Arc::new(move |run| {
                runtime
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .release_run_fence(
                        serde_json::to_value(run).map_err(|error| error.to_string())?,
                    )
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }),
            cancel_task,
        });
        let engine = EngineRunPause::new_with_barrier(state_dir, effects, barrier);
        (engine, run_pause)
    }
}

/// Adapt the canonical native child port to the task-record custody protocol.
struct NativeTaskPausePort;
impl senpi_task::manager::run_pause::TaskChildPausePort for NativeTaskPausePort {
    fn pause_for_run(
        &self,
        child: &Arc<dyn senpi_task::manager::ManagedChildHandle>,
        owner: &RunPauseOwner,
    ) -> Result<Option<senpi_task::manager::run_pause::EngineInheritReceipt>, String> {
        use senpi_task::pause::ports::ChildPausePort;
        match ChildPausePort::pause_for_run(
            &senpi_task::pause::native_child_pause::NativeChildPausePort,
            child.as_ref(),
            owner,
        )
        .map_err(|error| error.to_string())?
        {
            senpi_task::pause::ports::EngineInheritReceipt::Inherited { child_seq } => {
                Ok(Some(senpi_task::manager::run_pause::EngineInheritReceipt {
                    child_seq,
                }))
            }
            senpi_task::pause::ports::EngineInheritReceipt::WrongParent => Ok(None),
        }
    }
    fn observe_hold(
        &self,
        child: &Arc<dyn senpi_task::manager::ManagedChildHandle>,
        parent: &str,
    ) -> Option<senpi_task::manager::run_pause::ObservedHold> {
        match senpi_task::pause::ports::ChildPausePort::observe_hold(
            &senpi_task::pause::native_child_pause::NativeChildPausePort,
            child.as_ref(),
            parent,
        ) {
            Ok(hold) => hold.map(|hold| senpi_task::manager::run_pause::ObservedHold {
                held: hold.held,
                child_seq: hold.child_seq,
            }),
            Err(error) => {
                maho_ext_api::diagnostic(&format!("native pause observation failed: {error}"));
                None
            }
        }
    }
    fn resume_turn(
        &self,
        child: &Arc<dyn senpi_task::manager::ManagedChildHandle>,
        request: &senpi_task::manager::run_pause::ResumeTurnRequest,
    ) -> Result<senpi_task::manager::run_pause::ResumeTurnResult, String> {
        let result = senpi_task::pause::ports::ChildPausePort::resume_turn(
            &senpi_task::pause::native_child_pause::NativeChildPausePort,
            child.as_ref(),
            &senpi_task::pause::ports::ResumeTurnRequest {
                owner: request.owner.clone(),
                resume_id: request.resume_id.clone(),
                child_seq_cutoff: request.child_seq_cutoff,
            },
        )
        .map_err(|error| error.to_string())?;
        Ok(match result {
            senpi_task::pause::ports::ResumeChildTurnResult::Resumed(turn) => {
                senpi_task::manager::run_pause::ResumeTurnResult::Resumed {
                    turn_id: turn.turn_id,
                    outcome: turn.outcome,
                }
            }
            senpi_task::pause::ports::ResumeChildTurnResult::OtherHoldLive { child_seq } => {
                senpi_task::manager::run_pause::ResumeTurnResult::OtherHoldLive { child_seq }
            }
        })
    }
    fn hold_input(
        &self,
        child: &Arc<dyn senpi_task::manager::ManagedChildHandle>,
        input: &senpi_task::manager::run_pause::HeldTaskInput,
        owner: &RunPauseOwner,
    ) -> Result<senpi_task::manager::run_pause::EngineHoldInputResult, String> {
        let result = senpi_task::pause::ports::ChildPausePort::hold_input(
            &senpi_task::pause::native_child_pause::NativeChildPausePort,
            child.as_ref(),
            &senpi_task::pause::ports::HoldInput {
                input_id: input.input_id.clone(),
                revision: input.revision,
                text: input.text.clone(),
            },
            &owner.parent_session_id,
        )
        .map_err(|error| error.to_string())?;
        use senpi_task::manager::run_pause::EngineHoldInputResult as Task;
        use senpi_task::pause::ports::EngineHoldInputResult as Native;
        Ok(match result {
            Native::Held {
                entry_id,
                duplicate,
            } => Task::Held {
                entry_id,
                duplicate,
            },
            Native::AlreadyDelivered { revision } => Task::AlreadyDelivered { revision },
            Native::InputIdConflict { held_revision } => Task::InputIdConflict { held_revision },
        })
    }
}

#[cfg(test)]
mod parity_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn effects() -> EngineRunPauseEffects {
        EngineRunPauseEffects {
            sources: Arc::new(Mutex::new(vec![])),
            task_records: Arc::new(Vec::new),
            pause_task: Arc::new(|_, _| Ok(None)),
            resume_task: Arc::new(|_, _, _, _, _| Ok(ResumeParticipantResult::Settled)),
            observe_task_hold: Arc::new(|_, _| None),
            hold_task_input: Arc::new(|_, _| Err("unused".into())),
            fence_run: Arc::new(|_| Ok(())),
            release_fences: Arc::new(|_| Ok(())),
            cancel_task: Arc::new(|_| Ok(())),
        }
    }
    struct Source;
    impl RunPauseSource for Source {
        fn refs(&self, _: &str) -> Vec<RunRef> {
            vec![RunRef::task("source")]
        }
    }
    #[test]
    fn sources_registered_after_composition_join_the_same_controller_snapshot() {
        let dir = tempfile::tempdir().expect("dir");
        let effects = Arc::new(effects());
        let engine = EngineRunPause::new(dir.path(), effects.clone());
        assert!(engine.refs("parent").is_empty());
        engine.register(Arc::new(Source));
        assert_eq!(engine.refs("parent"), vec![RunRef::task("source")]);
        assert_eq!(effects.snapshot("parent", &[])[0].key, "run:task:source");
    }
    #[test]
    fn native_stop_bus_pauses_without_external_apply_and_ignores_fences() {
        let dir = tempfile::tempdir().expect("dir");
        let mut effects = effects();
        let (sent, received) = std::sync::mpsc::sync_channel(1);
        effects.fence_run = Arc::new(move |_| sent.send(()).map_err(|error| error.to_string()));
        let engine = Arc::new(EngineRunPause::new(dir.path(), Arc::new(effects)));
        engine.register(Arc::new(Source));
        let custody = crate::event_bridge::TaskPauseCustody {
            bridge: Arc::new(crate::run_pause_bridge::EngineRunPauseBridge::new(
                engine.clone(),
            )),
            session_id: Arc::new(|| Some("parent".into())),
        };
        let mut api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("task", "/tmp".into(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let _subscription = custody.register(&mut api);
        api.events.emit(
            "session:hold",
            &serde_json::json!({"type":"session_paused", "kind":"run_fence", "pauseSeq":1}),
        );
        assert_eq!(
            engine.admission_state("parent"),
            senpi_task::pause::admission_barrier::AdmissionState::Open
        );
        api.events.emit(
            "session:hold",
            &serde_json::json!({"type":"session_paused", "kind":"session_hold", "pauseSeq":2}),
        );
        received
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("Stop reached controller fence");
        assert_eq!(
            engine.admission_state("parent"),
            senpi_task::pause::admission_barrier::AdmissionState::Closed { pause_seq: 2 }
        );
        assert_eq!(
            engine.restore("parent").expect("durable pause").pause_seq,
            2
        );
    }
    #[test]
    fn run_fences_reach_the_host_and_failures_are_not_discarded() {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let mut effects = effects();
        effects.fence_run = Arc::new(move |run| {
            assert_eq!(run.id, "task");
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        futures::executor::block_on(effects.add_fences("parent", 1, vec![RunRef::task("task")]))
            .expect("fenced");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        effects.fence_run = Arc::new(|_| Err("host refused".into()));
        assert!(
            futures::executor::block_on(effects.add_fences(
                "parent",
                1,
                vec![RunRef::task("task")]
            ))
            .is_err()
        );
    }
}
