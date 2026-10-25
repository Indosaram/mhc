//! `engine-run-pause.ts`: persistent parent-run pause ownership over the owned child-session subtree.
//!
//! A Stop on the parent session must reach EVERY run this session owns - DAG runs, tasks, workpool
//! workers, team members - hold each child non-terminally, and keep the admission barrier closed so
//! no new child slips in behind it. A Resume reopens admission under a credential, releases each held
//! child, and installs the resumed runs before admission is released. A cancelled run leaves custody
//! and releases its fences.

use senpi_task::pause::registry::PauseError;
use std::sync::Arc;

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
}

/// The effects the engine hands the controller: every source plus the task manager's own ports.
pub struct EngineRunPauseEffects {
    pub sources: Vec<Arc<dyn RunPauseSource>>,
    pub pause_task: Arc<
        dyn Fn(
                &str,
                &RunPauseOwner,
            )
                -> Result<Option<senpi_task::manager::run_pause::EngineInheritReceipt>, String>
            + Send
            + Sync,
    >,
    pub resume_task: Arc<
        dyn Fn(
                &str,
                &RunPauseOwner,
                &str,
                Option<u64>,
                Option<&ResumeCredential>,
            ) -> Result<ResumeParticipantResult, String>
            + Send
            + Sync,
    >,
    pub observe_task_hold: Arc<
        dyn Fn(&str, &str) -> Option<senpi_task::manager::run_pause::ObservedHold> + Send + Sync,
    >,
    pub hold_task_input: Arc<
        dyn Fn(
                &str,
                &senpi_task::manager::run_pause::HeldTaskInput,
            ) -> Result<senpi_task::manager::run_pause::EngineHoldInputResult, String>
            + Send
            + Sync,
    >,
    pub fence_run: Arc<dyn Fn(&RunRef) + Send + Sync>,
    pub release_fences: Arc<dyn Fn(&RunRef) + Send + Sync>,
    pub cancel_task: Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>,
}

impl RunPauseEffects for EngineRunPauseEffects {
    fn snapshot(&self, parent: &str, admissions: &[AdmissionParticipant]) -> Vec<PauseParticipant> {
        let mut participants: Vec<_> = self
            .sources
            .iter()
            .flat_map(|source| source.snapshot(parent))
            .collect();
        participants.extend(admissions.iter().map(|admission| PauseParticipant {
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
        }));
        participants
    }
    fn quiesce<'a>(&'a self, parent: &'a str, seq: u64) -> Effect<'a, ()> {
        Box::pin(async move {
            for source in &self.sources {
                source.quiesce(parent, seq);
            }
            Ok(())
        })
    }
    fn add_fences<'a>(&'a self, _parent: &'a str, _seq: u64, runs: Vec<RunRef>) -> Effect<'a, ()> {
        Box::pin(async move {
            for run in runs {
                (self.fence_run)(&run);
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
            for source in &self.sources {
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
            for source in &self.sources {
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
            for source in &self.sources {
                if source.refs(parent).contains(&participant.run_ref) {
                    source
                        .cancel(parent, &participant.run_ref)
                        .map_err(PauseError::message)?;
                }
            }
            if let Some(task) = &participant.task_id {
                (self.cancel_task)(task).map_err(PauseError::message)?;
            }
            (self.release_fences)(&participant.run_ref);
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
    sources: Vec<Arc<dyn RunPauseSource>>,
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
        self.controller
            .released(parent_session_id, pause_seq, resume_id)
            .unwrap_or_else(|error| {
                maho_ext_api::diagnostic(&format!("pause release failed: {error}"));
                false
            })
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
        _record: &senpi_task::state::TaskRecord,
    ) -> Option<Result<Arc<dyn senpi_task::manager::ManagedChildHandle>, String>> {
        // No Rust runner rebuilds a paused child without starting a turn, so there is no restore port.
        None
    }

    fn track(
        &self,
        _task_id: &str,
        _handle: Arc<dyn senpi_task::manager::ManagedChildHandle>,
        _model: &str,
        _epoch: i64,
        _turn: senpi_task::manager::run_pause::TaskResumedTurn,
    ) {
        // The resumed turn's outcome is already subscribed by the handle; nothing to re-track.
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
        match senpi_task::pause::native_child_pause::NativeChildPausePort
            .observe_hold(child.as_ref(), parent)
        {
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
        let result = senpi_task::pause::native_child_pause::NativeChildPausePort
            .resume_turn(
                child.as_ref(),
                senpi_task::pause::ports::ResumeTurnRequest {
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
        let result = senpi_task::pause::native_child_pause::NativeChildPausePort
            .hold_input(
                child.as_ref(),
                senpi_task::pause::ports::HoldInput {
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
