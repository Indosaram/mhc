//! The task manager (`manager/manager.ts`). Manager state sits behind one mutex that is never held
//! across a handle, runner, store-transition or steering call, so child listeners may re-enter.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use serde_json::json;
use crate::host::HostError;

use crate::lifecycle::port::ReattachFailureKind;
use crate::lifecycle::{
    DestroyCause, LifecycleReattachPorts, ReattachResult, RespawnResult,
    register_lifecycle_reattach_ports,
};
use crate::manager::child_handle::{
    ManagedChildHandle, ManagedChildListener, Unsubscribe, discard_managed_handle,
};
use crate::manager::concurrency::{Grant, TaskConcurrency};
use crate::manager::continue_result::{ContinueResult, to_continue_result};
use crate::manager::depth_policy::{DepthDecision, DepthPolicyInput, decide_depth_policy};
use crate::manager::execution_mode::{ExecutionMode, ExecutionModeSources, resolve_execution_mode};
use crate::manager::helpers::{
    build_managed_spec, build_record_input, build_spawn_spec_v1, in_session, is_terminal_record,
    now_iso, record_spawned_pid,
};
use crate::manager::names::NameRegistry;
use crate::manager::outcome::{ErrorOutcomeInput, OutcomeTrackerPorts, track_outcome};
use crate::manager::respawn::{RespawnInput, respawn_managed_task};
use crate::manager::transcript_log::subscribe_transcript_log;
use crate::manager::types::{
    Clock, ListScope, ListedTask, ManagedRunner, ManagedRunnerError, ManagedStartSpec,
    ManagerStartSpec, NoopDestruction, OwnedStartResult, ResolvedChildPlan, RpcRespawnRunner,
    SpawnAdmission, StartFailure, StartResult, StartedTask, TaskManagerOptions,
};
use crate::run_stats::{RunStatsTracker, create_run_stats_tracker};
use crate::runners::RunnerFailureKind;
use crate::runners::types::RpcRunnerSpec;
use crate::shared::DagTaskOwner;
use crate::state::{
    ResidencyState, TaskRecord, TaskRunStats, TaskSpawnSpec, TaskStatus, TaskTransition,
    create_task_record, parse_task_id, resolved_reasoning_fields, sync_task_id_floor,
};
use crate::steering::{
    CancelOptions, CancelOutcome, DestructionPort, InterruptOutcome, SendInput, SendOutcome,
    SteeringEngine, SteeringError, SteeringPort,
};
use crate::store::{
    ClaimError, ClaimOptions, NameBinding, PersistedTaskEvent, StoreError, TaskRecordSaver,
    TaskRecordStore, claim_task_record, with_task_record_lock,
};

pub const GENERIC_START_FAILURE_MESSAGE: &str = "Task runner failed to start.";
pub const ID_CONTENTION_MESSAGE: &str =
    "task id allocation failed under contention; retry the spawn";
pub const SPAWN_BOOKKEEPING_FAILED: &str = "spawn bookkeeping failed";

type Tracker = RunStatsTracker<Box<dyn Fn() -> u64 + Send>>;

struct LiveTask {
    handle: Arc<dyn ManagedChildHandle>,
    model: String,
    unsubscribe: Option<Unsubscribe>,
    managed_spec: Option<ManagedStartSpec>,
    runner: Option<Arc<dyn ManagedRunner>>,
}

#[derive(Clone)]
struct LaunchContext {
    record: TaskRecord,
    managed_spec: ManagedStartSpec,
    runner: Arc<dyn ManagedRunner>,
    model: String,
}

/// A pending child subscriber: the listener plus its detach once a live handle carries it.
struct ChildSubscriber {
    id: u64,
    listener: ManagedChildListener,
    detach: Option<Unsubscribe>,
}

#[derive(Default)]
struct State {
    live: HashMap<String, LiveTask>,
    child_subscribers: HashMap<String, Vec<ChildSubscriber>>,
    released: HashMap<String, i64>,
    waiters: HashMap<String, Vec<(u64, Sender<TaskRecord>)>>,
    background: HashSet<String>,
    run_stats: HashMap<String, Tracker>,
    next_subscriber_id: u64,
    processed_outcomes: u64,
}

/// Why [`TaskManager::wait_for`] returned without a terminal record.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WaitError {
    #[error("waitFor aborted")]
    Aborted,
    #[error("waitFor timed out")]
    TimedOut,
    #[error(transparent)]
    InvalidTaskId(#[from] crate::state::InvalidTaskIdError),
}

/// A cooperative abort for [`TaskManager::wait_for`] (TS `AbortSignal`).
#[derive(Clone, Default)]
pub struct AbortSignal(Arc<Mutex<bool>>);

impl AbortSignal {
    pub fn abort(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
    }

    pub fn aborted(&self) -> bool {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct Inner {
    store: TaskRecordStore,
    saver: Arc<dyn TaskRecordSaver + Send + Sync>,
    runners: crate::manager::types::ManagedRunners,
    planner: crate::manager::types::ChildPlanner,
    config: crate::manager::types::ManagerConfig,
    cwd: String,
    now: Clock,
    destruction: Arc<dyn DestructionPort>,
    admit: Option<crate::manager::types::AdmitResident>,
    fallible_admit: Option<Arc<dyn Fn(&str) -> Result<SpawnAdmission, HostError> + Send + Sync>>,
    trusted_respawn_launch: Option<crate::manager::types::TrustedRespawnLaunchResolver>,
    host_pid: i64,
    rpc_respawn_runner: Arc<dyn RpcRespawnRunner>,
    names: Mutex<NameRegistry>,
    concurrency: Mutex<TaskConcurrency>,
    state: Mutex<State>,
    steering: SteeringEngine,
    this: Weak<Inner>,
}

/// Cheap cloneable handle onto one manager instance.
#[derive(Clone)]
pub struct TaskManager {
    inner: Arc<Inner>,
}

pub fn create_task_manager(options: TaskManagerOptions) -> TaskManager {
    TaskManager::new(options)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn log(message: &str, task_id: &str, error: &dyn std::fmt::Display) {
    utils::logger::log(
        message,
        Some(&json!({ "taskId": task_id, "error": error.to_string() })),
    );
}

pub fn public_start_failure_message(error: &ManagedRunnerError) -> &'static str {
    match error {
        ManagedRunnerError::Runner(failure) => match failure.kind {
            RunnerFailureKind::DepthExceeded => "In-process child depth limit exceeded.",
            RunnerFailureKind::SessionCreateFailed => "In-process child session creation failed.",
            RunnerFailureKind::ChildPromptFailed => "Child prompt failed to start.",
            _ => GENERIC_START_FAILURE_MESSAGE,
        },
        _ => GENERIC_START_FAILURE_MESSAGE,
    }
}

struct DefaultRpcRespawnRunner(crate::runners::rpc_process::RpcProcessRunner);

impl RpcRespawnRunner for DefaultRpcRespawnRunner {
    fn start(&self, spec: &RpcRunnerSpec) -> crate::manager::types::ManagedRunnerResult {
        self.0
            .start(spec)
            .map(|handle| handle as Arc<dyn ManagedChildHandle>)
            .map_err(ManagedRunnerError::Runner)
    }
}

fn owner_lock_path(state_dir: &Path, owner: &DagTaskOwner) -> Result<PathBuf, StoreError> {
    use sha2::{Digest, Sha256};
    let owner_key = format!("dag\0{}\0{}", owner.run_id, owner.node_id);
    let digest = Sha256::digest(owner_key.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let owner_dir = state_dir.join("owner-locks");
    std::fs::create_dir_all(&owner_dir)?;
    Ok(owner_dir.join(hex))
}

fn normalize_spec_name(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|trimmed| !trimmed.is_empty())
        .map(str::to_string)
}

impl TaskManager {
    pub fn new(options: TaskManagerOptions) -> Self {
        match options.store.list() {
            Ok(listed) => {
                if !listed.diagnostics.is_empty() {
                    utils::logger::log(
                        "senpi-task manager task record diagnostics while seeding id floor",
                        Some(&json!({ "count": listed.diagnostics.len() })),
                    );
                }
                if let Some(max_id) = listed.records.iter().map(|record| &record.task_id).max()
                    && let Ok(parsed) = parse_task_id(max_id)
                {
                    sync_task_id_floor(parsed);
                }
            }
            Err(error) => log(
                "senpi-task manager failed to seed task id floor",
                "",
                &error,
            ),
        }
        let now = options
            .now
            .unwrap_or_else(|| Arc::new(|| chrono::Utc::now().timestamp_millis()));
        let destruction = options
            .destruction
            .unwrap_or_else(|| Arc::new(NoopDestruction));
        let rpc_respawn_runner = options.rpc_respawn_runner.unwrap_or_else(|| {
            Arc::new(DefaultRpcRespawnRunner(
                crate::runners::rpc_process::RpcProcessRunner::new(Default::default()),
            ))
        });
        let saver: Arc<dyn TaskRecordSaver + Send + Sync> = options
            .record_saver
            .unwrap_or_else(|| Arc::new(options.store.clone()));
        let store = options.store;
        let inner = Arc::new_cyclic(|this: &Weak<Inner>| {
            let port = steering_port(
                this.clone(),
                store.clone(),
                destruction.clone(),
                now.clone(),
            );
            Inner {
                store: store.clone(),
                saver,
                runners: options.runners,
                planner: options.planner,
                cwd: options.cwd,
                concurrency: Mutex::new(TaskConcurrency::new(options.config.concurrency.clone())),
                config: options.config,
                now,
                destruction,
                admit: options.admit,
                fallible_admit: options.fallible_admit,
                trusted_respawn_launch: options.trusted_respawn_launch,
                host_pid: options
                    .host_pid
                    .unwrap_or_else(|| i64::from(std::process::id())),
                rpc_respawn_runner,
                names: Mutex::new(NameRegistry::default()),
                state: Mutex::new(State::default()),
                steering: SteeringEngine::new(port),
                this: this.clone(),
            }
        });
        let manager = Self { inner };
        let respawn_target = Arc::downgrade(&manager.inner);
        let reattach_target = Arc::downgrade(&manager.inner);
        register_lifecycle_reattach_ports(
            manager.inner.store.state_dir(),
            LifecycleReattachPorts {
                respawn: Arc::new(move |record, session_path| match respawn_target.upgrade() {
                    Some(inner) => TaskManager { inner }.respawn(record, session_path),
                    None => RespawnResult::Failed {
                        disposition: crate::lifecycle::port::RespawnDisposition::Retryable,
                        code: crate::lifecycle::port::RespawnFailureCode::RespawnFailed,
                        reason: "task manager is gone".to_string(),
                    },
                }),
                reattach: Arc::new(move |record, handle| match reattach_target.upgrade() {
                    Some(inner) => TaskManager { inner }.reattach(record, handle),
                    None => ReattachResult::Failed {
                        kind: ReattachFailureKind::Failed,
                        reason: "task manager is gone".to_string(),
                    },
                }),
            },
        );
        manager
    }

    pub fn store(&self) -> &TaskRecordStore {
        &self.inner.store
    }

    pub fn start(&self, spec: &ManagerStartSpec) -> StartResult {
        let plan = match (self.inner.planner)(spec) {
            Ok(plan) => plan,
            Err(error) => return StartResult::PlanUnresolved(*error),
        };
        if let Some(rejected) = self.admission_rejection(spec, &plan) {
            return rejected;
        }
        self.inner.start_resolved(spec, &plan, None)
    }

    pub fn start_owned(&self, spec: &ManagerStartSpec, owner: &DagTaskOwner) -> OwnedStartResult {
        let plan = match (self.inner.planner)(spec) {
            Ok(plan) => plan,
            Err(error) => return OwnedStartResult::NotStarted(StartResult::PlanUnresolved(*error)),
        };
        if let Some(rejected) = self.admission_rejection(spec, &plan) {
            return OwnedStartResult::NotStarted(rejected);
        }
        let lock_path = match owner_lock_path(self.inner.store.state_dir(), owner) {
            Ok(path) => path,
            Err(error) => {
                return OwnedStartResult::NotStarted(bookkeeping_failure(spec, &plan, error));
            }
        };
        let locked = with_task_record_lock(&lock_path, || {
            if let Some(raced) = self.owned_result(owner) {
                return Ok(raced);
            }
            Ok(match self.inner.start_resolved(spec, &plan, Some(owner)) {
                StartResult::Started(task) => OwnedStartResult::Started {
                    task,
                    reused: false,
                },
                other => OwnedStartResult::NotStarted(other),
            })
        });
        locked.unwrap_or_else(|error| {
            OwnedStartResult::NotStarted(bookkeeping_failure(spec, &plan, error))
        })
    }

    fn admission_rejection(&self, spec: &ManagerStartSpec, plan: &ResolvedChildPlan) -> Option<StartResult> {
        let admission = if let Some(admit) = &self.inner.fallible_admit {
            match admit(&spec.parent_session_id) {
                Ok(admission) => admission,
                Err(error) => return Some(StartResult::StartFailed(StartFailure {
                    task_id: String::new(), name: spec.name.clone().unwrap_or_default(),
                    category: spec.category.clone().or(plan.category.clone()),
                    subagent_type: spec.subagent_type.clone().or(plan.agent_type.clone()),
                    execution_mode: spec.execution_mode.unwrap_or_default(), model: plan.model.clone(),
                    resolved_model: plan.resolved_model.clone(), run_in_background: spec.run_in_background,
                    error_message: error.to_string(),
                })),
            }
        } else { (self.inner.admit.as_ref()?)(&spec.parent_session_id) };
        match admission {
            SpawnAdmission::Rejected { message } => {
                Some(StartResult::ResidencyDenied { reason: message })
            }
            SpawnAdmission::Admitted | SpawnAdmission::Evicted { .. } => None,
        }
    }

    pub fn find_owned_task(&self, run_id: &str, node_id: &str) -> Option<TaskRecord> {
        self.inner
            .store
            .list()
            .ok()?
            .records
            .into_iter()
            .find(|record| {
                record
                    .owner
                    .as_ref()
                    .is_some_and(|owner| owner.run_id == run_id && owner.node_id == node_id)
            })
    }

    fn owned_result(&self, owner: &DagTaskOwner) -> Option<OwnedStartResult> {
        let record = self.find_owned_task(&owner.run_id, &owner.node_id)?;
        let existing = record
            .owner
            .as_ref()
            .map(|owner| owner.fingerprint.clone())
            .unwrap_or_default();
        if existing != owner.fingerprint {
            return Some(OwnedStartResult::OwnerConflict {
                task_id: record.task_id,
                existing_fingerprint: existing,
                requested_fingerprint: owner.fingerprint.clone(),
            });
        }
        Some(OwnedStartResult::Started {
            task: StartedTask {
                name: record
                    .name
                    .clone()
                    .unwrap_or_else(|| record.task_id.clone()),
                task_id: record.task_id,
                status: record.status,
                resolved_model: record.resolved_model,
                queue_position: None,
                name_warning: None,
            },
            reused: true,
        })
    }

    pub fn continue_task(
        &self,
        task_id_or_name: &str,
        prompt: &str,
        deliver_as: Option<crate::state::DeliverAs>,
    ) -> Result<ContinueResult, SteeringError> {
        let outcome = self.inner.steering.send_to_task(&SendInput {
            id_or_name: task_id_or_name.to_string(),
            message: prompt.to_string(),
            deliver_as: Some(deliver_as.unwrap_or(crate::state::DeliverAs::FollowUp)),
            ..SendInput::default()
        })?;
        Ok(to_continue_result(outcome))
    }

    pub fn send_to_task(&self, input: &SendInput) -> Result<SendOutcome, SteeringError> {
        self.inner.steering.send_to_task(input)
    }

    pub fn interrupt_task(&self, id_or_name: &str) -> Result<InterruptOutcome, SteeringError> {
        let outcome = self.inner.steering.interrupt_task(id_or_name)?;
        if let InterruptOutcome::Interrupted { task_id, .. } = &outcome {
            self.inner.release_slot_for_task(task_id);
        }
        Ok(outcome)
    }

    pub fn cancel_task(
        &self,
        id_or_name: &str,
        reason: Option<&str>,
        options: CancelOptions,
    ) -> Result<CancelOutcome, SteeringError> {
        let outcome = self
            .inner
            .steering
            .cancel_task(id_or_name, reason, options)?;
        if let CancelOutcome::Cancelled { task_id, .. } = &outcome {
            self.inner.release_slot_for_task(task_id);
        }
        Ok(outcome)
    }

    pub fn get(&self, task_id: &str) -> Option<TaskRecord> {
        self.inner.try_load(task_id)
    }

    pub fn list(&self, scope: &ListScope) -> Vec<ListedTask> {
        let records = match self.inner.store.list() {
            Ok(listed) => listed.records,
            Err(_) => return Vec::new(),
        };
        let concurrency = lock(&self.inner.concurrency);
        records
            .into_iter()
            .filter(|record| match scope {
                ListScope::All => true,
                ListScope::ParentSession(session_id) => in_session(record, session_id),
            })
            .map(|record| {
                let queue_position = if record.status == TaskStatus::Pending {
                    concurrency.queue_position(&record.model, &record.task_id)
                } else {
                    None
                };
                ListedTask {
                    record,
                    queue_position,
                }
            })
            .collect()
    }

    /// Blocks until the task is terminal; `timeout` bounds the wait (TS waits unbounded).
    pub fn wait_for(
        &self,
        task_id: &str,
        signal: Option<&AbortSignal>,
        timeout: Option<Duration>,
    ) -> Result<TaskRecord, WaitError> {
        if signal.is_some_and(AbortSignal::aborted) {
            return Err(WaitError::Aborted);
        }
        let id = parse_task_id(task_id)?.to_string();
        let (waiter_id, receiver) = {
            let mut state = lock(&self.inner.state);
            if let Some(current) = self.inner.try_load(&id)
                && is_terminal_record(&current)
            {
                return Ok(current);
            }
            let (sender, receiver) = channel();
            state.next_subscriber_id += 1;
            let waiter_id = state.next_subscriber_id;
            state
                .waiters
                .entry(id.clone())
                .or_default()
                .push((waiter_id, sender));
            (waiter_id, receiver)
        };
        let deadline = timeout.map(|timeout| std::time::Instant::now() + timeout);
        loop {
            match receiver.recv_timeout(Duration::from_millis(10)) {
                Ok(record) => return Ok(record),
                Err(RecvTimeoutError::Disconnected) => return Err(WaitError::Aborted),
                Err(RecvTimeoutError::Timeout) => {}
            }
            let expired = deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline);
            if expired || signal.is_some_and(AbortSignal::aborted) {
                self.inner.remove_waiter(&id, waiter_id);
                if let Ok(record) = receiver.try_recv() {
                    return Ok(record);
                }
                return Err(if expired {
                    WaitError::TimedOut
                } else {
                    WaitError::Aborted
                });
            }
        }
    }

    pub fn waiter_key_count(&self) -> usize {
        lock(&self.inner.state).waiters.len()
    }

    pub fn waiter_count(&self, task_id: &str) -> usize {
        lock(&self.inner.state)
            .waiters
            .get(task_id)
            .map_or(0, Vec::len)
    }

    /// Tracked child outcomes the manager has applied or ignored so far.
    pub fn processed_outcomes(&self) -> u64 {
        lock(&self.inner.state).processed_outcomes
    }

    pub fn released_key_count(&self) -> usize {
        lock(&self.inner.state).released.len()
    }

    pub fn run_stats_snapshot(&self, task_id: &str) -> Option<TaskRunStats> {
        self.inner.run_stats_snapshot(task_id)
    }

    pub fn forget(&self, task_id: &str) {
        let (unsubscribe, detaches) = {
            let mut state = lock(&self.inner.state);
            let unsubscribe = state
                .live
                .remove(task_id)
                .and_then(|mut live| live.unsubscribe.take());
            let detaches: Vec<Unsubscribe> = state
                .child_subscribers
                .remove(task_id)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|subscriber| subscriber.detach)
                .collect();
            state.background.remove(task_id);
            state.released.remove(task_id);
            state.run_stats.remove(task_id);
            (unsubscribe, detaches)
        };
        if let Some(unsubscribe) = unsubscribe {
            unsubscribe();
        }
        for detach in detaches {
            detach();
        }
        if let Err(error) = self.inner.steering.drop_pending(task_id) {
            log("senpi-task manager drop pending failed", task_id, &error);
        }
    }

    pub fn get_resident_handle(&self, task_id: &str) -> Option<Arc<dyn ManagedChildHandle>> {
        self.inner.live_handle(task_id)
    }

    pub fn subscribe_child(&self, task_id: &str, listener: ManagedChildListener) -> Unsubscribe {
        if let Some(live) = self.inner.live_handle(task_id) {
            return live.subscribe(listener);
        }
        let id = {
            let mut state = lock(&self.inner.state);
            state.next_subscriber_id += 1;
            let id = state.next_subscriber_id;
            state
                .child_subscribers
                .entry(task_id.to_string())
                .or_default()
                .push(ChildSubscriber {
                    id,
                    listener,
                    detach: None,
                });
            id
        };
        let weak = Arc::downgrade(&self.inner);
        let task_id = task_id.to_string();
        Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.remove_child_subscriber(&task_id, id);
            }
        })
    }

    pub fn resident_task_ids(&self) -> Vec<String> {
        lock(&self.inner.state).live.keys().cloned().collect()
    }

    pub fn promote_to_background(&self, task_id: &str) -> bool {
        let promoted = !self.was_background(task_id);
        lock(&self.inner.state)
            .background
            .insert(task_id.to_string());
        if let Err(error) = self.inner.store.mutate(task_id, |record| {
            if record.notify_on_terminal {
                record.clone()
            } else {
                TaskRecord {
                    notify_on_terminal: true,
                    ..record.clone()
                }
            }
        }) {
            log("senpi-task manager promote failed", task_id, &error);
        }
        promoted
    }

    pub fn was_background(&self, task_id: &str) -> bool {
        match self.inner.try_load(task_id) {
            Some(record) => record.notify_on_terminal,
            None => lock(&self.inner.state).background.contains(task_id),
        }
    }

    pub fn respawn(
        &self,
        record: &TaskRecord,
        resume_session_path: Option<&Path>,
    ) -> RespawnResult {
        respawn_managed_task(&RespawnInput {
            record,
            session_path: resume_session_path,
            state_dir: self.inner.store.state_dir(),
            runners: &self.inner.runners,
            rpc_runner: self.inner.rpc_respawn_runner.as_ref(),
            trusted_launch: self.inner.trusted_respawn_launch.as_ref(),
        })
    }

    pub fn reattach(
        &self,
        record: &TaskRecord,
        handle: Arc<dyn ManagedChildHandle>,
    ) -> ReattachResult {
        self.inner.reattach(record, handle)
    }
}

fn bookkeeping_failure(
    spec: &ManagerStartSpec,
    plan: &ResolvedChildPlan,
    error: impl std::fmt::Display,
) -> StartResult {
    log("senpi-task manager owner lock failed", "", &error);
    StartResult::StartFailed(StartFailure {
        task_id: String::new(),
        name: normalize_spec_name(spec.name.as_deref()).unwrap_or_default(),
        category: spec.category.clone().or(plan.category.clone()),
        subagent_type: spec.subagent_type.clone().or(plan.agent_type.clone()),
        execution_mode: spec.execution_mode.unwrap_or_default(),
        model: plan.model.clone(),
        resolved_model: plan.resolved_model.clone(),
        run_in_background: spec.run_in_background,
        error_message: SPAWN_BOOKKEEPING_FAILED.to_string(),
    })
}

fn steering_port(
    this: Weak<Inner>,
    store: TaskRecordStore,
    destruction: Arc<dyn DestructionPort>,
    now: Clock,
) -> SteeringPort {
    let live = this.clone();
    let dequeue = this.clone();
    let revive = this.clone();
    let stats = this;
    SteeringPort {
        store,
        live_handle: Arc::new(move |task_id| live.upgrade()?.live_handle(task_id)),
        dequeue_pending: Arc::new(move |task_id| {
            dequeue
                .upgrade()
                .is_some_and(|inner| inner.dequeue_pending(task_id))
        }),
        reacquire_for_revive: Arc::new(move |task_id| {
            if let Some(inner) = revive.upgrade() {
                inner.reacquire_for_revive(task_id);
            }
        }),
        destruction,
        run_stats_snapshot: Arc::new(move |task_id| stats.upgrade()?.run_stats_snapshot(task_id)),
        now,
    }
}

impl Inner {
    fn arc(&self) -> Arc<Inner> {
        self.this
            .upgrade()
            .expect("task manager is alive while in use")
    }

    fn now_ms(&self) -> i64 {
        (self.now)()
    }

    fn now_iso(&self) -> String {
        now_iso(self.now_ms())
    }

    fn try_load(&self, task_id: &str) -> Option<TaskRecord> {
        self.store.load(task_id).ok().flatten()
    }

    fn live_handle(&self, task_id: &str) -> Option<Arc<dyn ManagedChildHandle>> {
        lock(&self.state)
            .live
            .get(task_id)
            .map(|live| Arc::clone(&live.handle))
    }

    fn run_stats_snapshot(&self, task_id: &str) -> Option<TaskRunStats> {
        let now = u64::try_from(self.now_ms()).unwrap_or_default();
        lock(&self.state)
            .run_stats
            .get(task_id)
            .map(|tracker| tracker.snapshot(now))
    }

    fn drop_pending(&self, task_id: &str) {
        if let Err(error) = self.steering.drop_pending(task_id) {
            log("senpi-task manager drop pending failed", task_id, &error);
        }
    }

    fn transition(&self, task_id: &str, transition: &TaskTransition) -> bool {
        match self.store.transition(task_id, transition) {
            Ok(result) => result.applied,
            Err(error) => {
                log("senpi-task manager transition failed", task_id, &error);
                false
            }
        }
    }

    fn start_resolved(
        &self,
        spec: &ManagerStartSpec,
        plan: &ResolvedChildPlan,
        owner: Option<&DagTaskOwner>,
    ) -> StartResult {
        let max_depth = plan.max_depth.unwrap_or(self.config.max_depth);
        let allowed_subagents: Vec<String> = spec
            .allowed_subagents
            .iter()
            .flatten()
            .chain(plan.allowed_subagents.iter().flatten())
            .cloned()
            .collect();
        let target_agent_type = spec.subagent_type.as_deref().or(plan.agent_type.as_deref());
        let decision = decide_depth_policy(&DepthPolicyInput {
            child_depth: spec.depth,
            max_depth,
            target_agent_type,
            allowed_subagents: Some(&allowed_subagents),
        });
        if let DepthDecision::Denied { reason } = decision {
            return StartResult::DepthDenied {
                reason,
                child_depth: spec.depth,
                max_depth,
            };
        }
        let execution_mode = resolve_execution_mode(ExecutionModeSources {
            spec_mode: spec.execution_mode,
            agent_mode: plan.agent_execution_mode,
            config_mode: Some(self.config.default_execution_mode),
        });
        let requested_name = normalize_spec_name(spec.name.as_deref());
        let requested_registration = requested_name
            .as_deref()
            .map(|name| lock(&self.names).register(&spec.parent_session_id, Some(name), None));
        let claimed = match self.claim(
            spec,
            plan,
            execution_mode,
            owner,
            requested_registration.as_ref(),
        ) {
            Ok(record) => record,
            Err(error) => {
                if !matches!(
                    error,
                    ClaimError::Exhausted(_) | ClaimError::Store(StoreError::Collision { .. })
                ) {
                    log("senpi-task manager claim failed", "", &error);
                }
                if let Some(registration) = &requested_registration {
                    lock(&self.names).release(&spec.parent_session_id, &registration.name);
                }
                return StartResult::StartFailed(StartFailure {
                    task_id: String::new(),
                    name: requested_name.unwrap_or_default(),
                    category: spec.category.clone().or(plan.category.clone()),
                    subagent_type: spec.subagent_type.clone().or(plan.agent_type.clone()),
                    execution_mode,
                    model: plan.model.clone(),
                    resolved_model: plan.resolved_model.clone(),
                    run_in_background: spec.run_in_background,
                    error_message: ID_CONTENTION_MESSAGE.to_string(),
                });
            }
        };
        let registration = requested_registration.unwrap_or_else(|| {
            lock(&self.names).register(&spec.parent_session_id, None, Some(&claimed.task_id))
        });
        let claimed_name = claimed.name.clone().unwrap_or_default();
        let renamed = if registration.name == claimed_name {
            claimed.clone()
        } else {
            TaskRecord {
                name: Some(registration.name.clone()),
                ..claimed.clone()
            }
        };
        let managed_spec =
            build_managed_spec(&renamed, spec, plan, &self.cwd, self.store.state_dir());
        let final_record = TaskRecord {
            spawn_spec: Some(TaskSpawnSpec::V1(build_spawn_spec_v1(&managed_spec))),
            ..renamed
        };
        if let Err(error) = self.store.replace(&final_record) {
            log(
                "senpi-task manager spawn bookkeeping failed",
                &claimed.task_id,
                &error,
            );
            return self.bookkeeping_failed(spec, &claimed, &registration.name, execution_mode);
        }
        if spec.run_in_background {
            lock(&self.state)
                .background
                .insert(final_record.task_id.clone());
        }
        let context = LaunchContext {
            runner: Arc::clone(self.runners.get(execution_mode)),
            model: plan.model.clone(),
            managed_spec,
            record: final_record.clone(),
        };
        let started = |status: TaskStatus, queue_position: Option<usize>| {
            StartResult::Started(StartedTask {
                task_id: final_record.task_id.clone(),
                status,
                name: registration.name.clone(),
                resolved_model: plan.resolved_model.clone(),
                queue_position,
                name_warning: registration.warning.clone(),
            })
        };
        let immediate = {
            let mut concurrency = lock(&self.concurrency);
            if concurrency.has_free_slot(&plan.model) {
                concurrency.acquire(&plan.model, &final_record.task_id);
                None
            } else {
                let inner = self.arc();
                let queued = context.clone();
                let grant: Grant = Box::new(move || {
                    let _outcome_recorded_on_record = inner.launch(&queued);
                });
                Some(concurrency.enqueue(&plan.model, &final_record.task_id, grant))
            }
        };
        if let Some(position) = immediate {
            return started(TaskStatus::Pending, Some(position));
        }
        match self.launch(&context) {
            Ok(()) => started(TaskStatus::Running, None),
            Err(error_message) => StartResult::StartFailed(StartFailure {
                task_id: final_record.task_id.clone(),
                name: registration.name.clone(),
                category: final_record.category.clone(),
                subagent_type: final_record.agent_type.clone(),
                execution_mode,
                model: final_record.model.clone(),
                resolved_model: final_record.resolved_model.clone(),
                run_in_background: spec.run_in_background,
                error_message,
            }),
        }
    }

    fn claim(
        &self,
        spec: &ManagerStartSpec,
        plan: &ResolvedChildPlan,
        execution_mode: ExecutionMode,
        owner: Option<&DagTaskOwner>,
        requested: Option<&crate::manager::names::NameRegistration>,
    ) -> Result<TaskRecord, ClaimError> {
        let input = crate::state::TaskRecordInput {
            owner: owner.cloned(),
            ..build_record_input(spec, plan, "", execution_mode)
        };
        let draft = create_task_record(input, u64::try_from(self.now_ms()).ok())?;
        let claim_draft = TaskRecord {
            name: Some(
                requested
                    .map(|registration| registration.name.clone())
                    .unwrap_or_else(|| draft.task_id.clone()),
            ),
            host_pid: Some(self.host_pid),
            ..draft
        };
        let parent = spec.parent_session_id.clone();
        let name_available = |name: &str| lock(&self.names).is_available(&parent, name);
        let options = ClaimOptions {
            name_binding: if requested.is_none() {
                NameBinding::FollowsId
            } else {
                NameBinding::Preserve
            },
            name_available: requested
                .is_none()
                .then_some(&name_available as &dyn Fn(&str) -> bool),
            ..ClaimOptions::default()
        };
        claim_task_record(self.saver.as_ref(), &claim_draft, &options)
    }

    fn bookkeeping_failed(
        &self,
        spec: &ManagerStartSpec,
        claimed: &TaskRecord,
        name: &str,
        execution_mode: ExecutionMode,
    ) -> StartResult {
        if Some(name) != claimed.name.as_deref() {
            lock(&self.names).release(&spec.parent_session_id, name);
        }
        lock(&self.state).background.remove(&claimed.task_id);
        let timestamp = self.now_iso();
        let started = self.transition(
            &claimed.task_id,
            &TaskTransition::Start {
                timestamp: timestamp.clone(),
                pid: None,
                child_session_id: None,
            },
        );
        let failed = self.transition(
            &claimed.task_id,
            &TaskTransition::Fail {
                timestamp,
                error_message: SPAWN_BOOKKEEPING_FAILED.to_string(),
                killed: false,
                run_stats: None,
            },
        );
        assert!(
            started && failed,
            "spawn bookkeeping failure transitions were not applied"
        );
        StartResult::StartFailed(StartFailure {
            task_id: claimed.task_id.clone(),
            name: name.to_string(),
            category: claimed.category.clone(),
            subagent_type: claimed.agent_type.clone(),
            execution_mode,
            model: claimed.model.clone(),
            resolved_model: claimed.resolved_model.clone(),
            run_in_background: spec.run_in_background,
            error_message: SPAWN_BOOKKEEPING_FAILED.to_string(),
        })
    }

    fn launch(&self, context: &LaunchContext) -> Result<(), String> {
        let record = &context.record;
        let epoch = record.notification.run_epoch;
        let applied = self.transition(
            &record.task_id,
            &TaskTransition::Start {
                timestamp: self.now_iso(),
                pid: None,
                child_session_id: None,
            },
        );
        if !applied {
            self.release_slot(&record.task_id, &context.model, epoch);
            self.drop_pending(&record.task_id);
            self.settle_waiters(&record.task_id);
            return Err("task was cancelled before launch".to_string());
        }
        let handle = match context.runner.start(&context.managed_spec) {
            Ok(handle) => handle,
            Err(error) => {
                let message = public_start_failure_message(&error);
                self.release_slot(&record.task_id, &context.model, epoch);
                self.transition(
                    &record.task_id,
                    &TaskTransition::Fail {
                        timestamp: self.now_iso(),
                        error_message: message.to_string(),
                        killed: false,
                        run_stats: None,
                    },
                );
                if let Err(error) = self.store.append_event(
                    &record.task_id,
                    &PersistedTaskEvent {
                        event_type: "task_start_failed".to_string(),
                        payload: json!({ "error_message": message }),
                    },
                ) {
                    log(
                        "senpi-task manager start-failed event failed",
                        &record.task_id,
                        &error,
                    );
                }
                self.drop_pending(&record.task_id);
                self.settle_waiters(&record.task_id);
                return Err(message.to_string());
            }
        };
        if self
            .try_load(&record.task_id)
            .is_some_and(|current| current.status == TaskStatus::Cancelled)
        {
            lock(&self.state).live.insert(
                record.task_id.clone(),
                LiveTask {
                    handle,
                    model: context.model.clone(),
                    unsubscribe: None,
                    managed_spec: None,
                    runner: None,
                },
            );
            if let Err(error) = self
                .destruction
                .destroy_resident_task(&record.task_id, DestroyCause::Cancel)
            {
                log(
                    "senpi-task manager cancel-during-launch destroy failed",
                    &record.task_id,
                    &error,
                );
            }
            self.release_slot(&record.task_id, &context.model, epoch);
            self.settle_waiters(&record.task_id);
            return Err("task was cancelled during launch".to_string());
        }
        self.attach_launched(context, &handle);
        if let Err(error) = self.steering.notify_started(&record.task_id) {
            log(
                "senpi-task manager notify started failed",
                &record.task_id,
                &error,
            );
        }
        Ok(())
    }

    fn attach_launched(&self, context: &LaunchContext, handle: &Arc<dyn ManagedChildHandle>) {
        let task_id = &context.record.task_id;
        let unsubscribe = self.subscribe_child_facts(handle, task_id);
        lock(&self.state).live.insert(
            task_id.clone(),
            LiveTask {
                handle: Arc::clone(handle),
                model: context.model.clone(),
                unsubscribe: Some(unsubscribe),
                managed_spec: Some(context.managed_spec.clone()),
                runner: Some(Arc::clone(&context.runner)),
            },
        );
        self.attach_child_subscribers(task_id, handle);
        self.record_spawn_facts(task_id, handle.as_ref());
        track_outcome(
            self.arc(),
            task_id.clone(),
            Arc::clone(handle),
            context.model.clone(),
            context.record.notification.run_epoch,
        );
    }

    fn attach_child_subscribers(&self, task_id: &str, handle: &Arc<dyn ManagedChildHandle>) {
        let pending: Vec<(u64, ManagedChildListener)> = lock(&self.state)
            .child_subscribers
            .get(task_id)
            .map(|subscribers| {
                subscribers
                    .iter()
                    .map(|subscriber| (subscriber.id, Arc::clone(&subscriber.listener)))
                    .collect()
            })
            .unwrap_or_default();
        for (id, listener) in pending {
            let detach = handle.subscribe(listener);
            let mut state = lock(&self.state);
            let slot = state
                .child_subscribers
                .get_mut(task_id)
                .and_then(|subscribers| {
                    subscribers
                        .iter_mut()
                        .find(|subscriber| subscriber.id == id)
                });
            match slot {
                Some(subscriber) => {
                    let previous = subscriber.detach.replace(detach);
                    drop(state);
                    if let Some(previous) = previous {
                        previous();
                    }
                }
                None => {
                    drop(state);
                    detach();
                }
            }
        }
    }

    fn remove_child_subscriber(&self, task_id: &str, id: u64) {
        let detach = {
            let mut state = lock(&self.state);
            let Some(subscribers) = state.child_subscribers.get_mut(task_id) else {
                return;
            };
            let Some(index) = subscribers
                .iter()
                .position(|subscriber| subscriber.id == id)
            else {
                return;
            };
            let removed = subscribers.remove(index);
            if subscribers.is_empty() {
                state.child_subscribers.remove(task_id);
            }
            removed.detach
        };
        if let Some(detach) = detach {
            detach();
        }
    }

    fn record_spawn_facts(&self, task_id: &str, handle: &dyn ManagedChildHandle) {
        let Some(current) = self.try_load(task_id) else {
            return;
        };
        if is_terminal_record(&current) {
            return;
        }
        let with_pid =
            record_spawned_pid(&current, handle.pid()).unwrap_or_else(|| current.clone());
        let keeps_v1 = current
            .spawn_spec
            .as_ref()
            .is_some_and(|spec| spec.as_v1().is_some());
        let updated = match handle.spawn_spec() {
            Some(spawn) if !keeps_v1 => TaskRecord {
                spawn_spec: Some(TaskSpawnSpec::LegacyProcess {
                    cwd: spawn.cwd,
                    extensions: spawn.extensions,
                    member_env: spawn.member_env.map(|env| env.into_iter().collect()),
                }),
                ..with_pid
            },
            _ => with_pid,
        };
        if updated != current
            && let Err(error) = self.store.replace(&updated)
        {
            log("senpi-task manager spawn facts failed", task_id, &error);
        }
    }

    fn subscribe_child_facts(
        &self,
        handle: &Arc<dyn ManagedChildHandle>,
        task_id: &str,
    ) -> Unsubscribe {
        let transcript =
            subscribe_transcript_log(handle.as_ref(), Arc::new(self.store.clone()), task_id);
        let now = self.now.clone();
        let clock: Box<dyn Fn() -> u64 + Send> =
            Box::new(move || u64::try_from(now()).unwrap_or_default());
        let started_at = u64::try_from(self.now_ms()).unwrap_or_default();
        lock(&self.state).run_stats.insert(
            task_id.to_string(),
            create_run_stats_tracker(started_at, clock),
        );
        let weak = self.this.clone();
        let stats_task = task_id.to_string();
        let stats = handle.subscribe(Arc::new(move |event| {
            if let Some(inner) = weak.upgrade()
                && let Some(tracker) = lock(&inner.state).run_stats.get_mut(&stats_task)
            {
                tracker.accept(event);
            }
        }));
        Box::new(move || {
            transcript();
            stats();
        })
    }

    fn fallback_candidate(
        &self,
        input: &ErrorOutcomeInput,
    ) -> Option<(
        TaskRecord,
        crate::state::ResolvedModelRecord,
        ManagedStartSpec,
        Arc<dyn ManagedRunner>,
    )> {
        let retryable = !input.killed
            && matches!(
                input.failure.kind,
                RunnerFailureKind::ChildTurnFailed | RunnerFailureKind::ChildPromptFailed
            )
            && input.run_stats.as_ref().map_or(0, |stats| stats.tool_calls) == 0;
        if !retryable {
            return None;
        }
        let record = self.try_load(&input.task_id)?;
        let next_model = record.fallback_models.as_ref()?.first()?.clone();
        let state = lock(&self.state);
        let live = state.live.get(&input.task_id)?;
        if !Arc::ptr_eq(&live.handle, &input.handle) {
            return None;
        }
        let managed_spec = live.managed_spec.clone()?;
        let runner = Arc::clone(live.runner.as_ref()?);
        drop(state);
        Some((record, next_model, managed_spec, runner))
    }

    fn try_runtime_fallback(&self, input: &ErrorOutcomeInput) -> bool {
        let Some((record, next_model, managed_spec, runner)) = self.fallback_candidate(input)
        else {
            return false;
        };
        if let Err(error) = self
            .destruction
            .destroy_resident_task(&input.task_id, DestroyCause::FallbackHandoff)
        {
            log(
                "senpi-task manager fallback handoff destroy failed",
                &input.task_id,
                &error,
            );
        }
        let unsubscribe = lock(&self.state)
            .live
            .remove(&input.task_id)
            .and_then(|mut live| live.unsubscribe.take());
        if let Some(unsubscribe) = unsubscribe {
            unsubscribe();
        }
        self.release_slot(&input.task_id, &input.model, input.epoch);
        let remaining: Vec<_> = record
            .fallback_models
            .iter()
            .flatten()
            .skip(1)
            .cloned()
            .collect();
        let mut attempts = record
            .fallback_attempts
            .clone()
            .unwrap_or_else(|| record.resolved_model.iter().cloned().collect());
        attempts.push(next_model.clone());
        let mut notification = record.notification.clone();
        notification.run_epoch += 1;
        let next_record = TaskRecord {
            model: next_model.display.clone(),
            resolved_model: Some(next_model.clone()),
            fallback_models: Some(remaining.clone()),
            fallback_attempts: Some(attempts),
            updated_at: input.timestamp.clone(),
            notification,
            ..record.clone()
        };
        if let Err(error) = self.store.replace(&next_record) {
            log(
                "senpi-task manager fallback record failed",
                &input.task_id,
                &error,
            );
        }
        if let Err(error) = self.store.append_event(
            &input.task_id,
            &PersistedTaskEvent {
                event_type: "task_model_fallback".to_string(),
                payload: json!({
                    "from_model": record.model,
                    "to_model": next_model.display,
                    "error_message": input.failure.message,
                }),
            },
        ) {
            log(
                "senpi-task manager fallback event failed",
                &input.task_id,
                &error,
            );
        }
        let (_reasoning, variant) = resolved_reasoning_fields(&next_model);
        let next_spec = ManagedStartSpec {
            model: Some(next_model.display.clone()),
            requested_model: record.requested_model.clone(),
            fallback_models: Some(remaining),
            variant: variant.or(managed_spec.variant.clone()),
            ..managed_spec
        };
        let context = LaunchContext {
            record: next_record,
            managed_spec: next_spec,
            runner,
            model: next_model.display.clone(),
        };
        let inner = self.arc();
        let grant: Grant = Box::new(move || inner.launch_runtime_fallback(&context));
        let immediate = {
            let mut concurrency = lock(&self.concurrency);
            if concurrency.has_free_slot(&next_model.display) {
                concurrency.acquire(&next_model.display, &input.task_id);
                Some(grant)
            } else {
                concurrency.enqueue(&next_model.display, &input.task_id, grant);
                None
            }
        };
        if let Some(grant) = immediate {
            std::thread::spawn(grant);
        }
        true
    }

    fn launch_runtime_fallback(&self, context: &LaunchContext) {
        let task_id = &context.record.task_id;
        let epoch = context.record.notification.run_epoch;
        if self
            .try_load(task_id)
            .is_none_or(|current| current.status != TaskStatus::Running)
        {
            self.release_slot(task_id, &context.model, epoch);
            self.settle_waiters(task_id);
            return;
        }
        match context.runner.start(&context.managed_spec) {
            Ok(handle) => self.attach_launched(context, &handle),
            Err(error) => {
                let message = public_start_failure_message(&error);
                self.release_slot(task_id, &context.model, epoch);
                self.transition(
                    task_id,
                    &TaskTransition::Fail {
                        timestamp: self.now_iso(),
                        error_message: message.to_string(),
                        killed: false,
                        run_stats: None,
                    },
                );
                self.settle_waiters(task_id);
            }
        }
    }

    fn dequeue_pending(&self, task_id: &str) -> bool {
        let Some(record) = self.try_load(task_id) else {
            return false;
        };
        let removed = lock(&self.concurrency).remove(&record.model, task_id);
        lock(&self.state).background.remove(task_id);
        self.settle_waiters(task_id);
        removed
    }

    fn reacquire_for_revive(&self, task_id: &str) {
        let Some((handle, model)) = lock(&self.state)
            .live
            .get(task_id)
            .map(|live| (Arc::clone(&live.handle), live.model.clone()))
        else {
            return;
        };
        let epoch = self
            .try_load(task_id)
            .map_or(0, |record| record.notification.run_epoch);
        lock(&self.concurrency).acquire(&model, task_id);
        let now = self.now.clone();
        let clock: Box<dyn Fn() -> u64 + Send> =
            Box::new(move || u64::try_from(now()).unwrap_or_default());
        let started_at = u64::try_from(self.now_ms()).unwrap_or_default();
        lock(&self.state).run_stats.insert(
            task_id.to_string(),
            create_run_stats_tracker(started_at, clock),
        );
        track_outcome(self.arc(), task_id.to_string(), handle, model, epoch);
    }

    fn release_slot(&self, task_id: &str, model: &str, epoch: i64) {
        {
            let mut state = lock(&self.state);
            if state
                .released
                .get(task_id)
                .is_some_and(|released| *released >= epoch)
            {
                return;
            }
            state.released.insert(task_id.to_string(), epoch);
        }
        let grant = lock(&self.concurrency).release(model);
        if let Some(grant) = grant {
            std::thread::spawn(grant);
        }
    }

    fn release_slot_for_task(&self, task_id: &str) {
        let Some(model) = lock(&self.state)
            .live
            .get(task_id)
            .map(|live| live.model.clone())
        else {
            return;
        };
        let epoch = self
            .try_load(task_id)
            .map_or(0, |record| record.notification.run_epoch);
        self.release_slot(task_id, &model, epoch);
    }

    fn settle_waiters(&self, task_id: &str) {
        let Some(record) = self.try_load(task_id) else {
            return;
        };
        let waiters = lock(&self.state)
            .waiters
            .remove(task_id)
            .unwrap_or_default();
        for (_, waiter) in waiters {
            let _receiver_gone = waiter.send(record.clone()).is_err();
        }
    }

    fn remove_waiter(&self, task_id: &str, waiter_id: u64) {
        let mut state = lock(&self.state);
        if let Some(list) = state.waiters.get_mut(task_id) {
            list.retain(|(id, _)| *id != waiter_id);
            if list.is_empty() {
                state.waiters.remove(task_id);
            }
        }
    }

    fn reattach(&self, record: &TaskRecord, handle: Arc<dyn ManagedChildHandle>) -> ReattachResult {
        let discard = |handle: &Arc<dyn ManagedChildHandle>| {
            if let Err(error) = discard_managed_handle(handle.as_ref()) {
                log(
                    "senpi-task reattach discard failed",
                    handle.task_id(),
                    &error,
                );
            }
        };
        let fresh = self.try_load(&record.task_id);
        let Some(fresh) = fresh.filter(|fresh| {
            fresh.host_pid == Some(self.host_pid)
                && fresh.residency_state == ResidencyState::Resident
        }) else {
            discard(&handle);
            return ReattachResult::Failed {
                kind: ReattachFailureKind::Failed,
                reason: "task ownership claim is not held by this host".to_string(),
            };
        };
        if lock(&self.state).live.contains_key(&fresh.task_id) {
            discard(&handle);
            return ReattachResult::Failed {
                kind: ReattachFailureKind::AlreadyAttached,
                reason: "task already has a live handle".to_string(),
            };
        }
        let unsubscribe = self.subscribe_child_facts(&handle, &fresh.task_id);
        lock(&self.state).live.insert(
            fresh.task_id.clone(),
            LiveTask {
                handle: Arc::clone(&handle),
                model: fresh.model.clone(),
                unsubscribe: Some(unsubscribe),
                managed_spec: None,
                runner: None,
            },
        );
        self.attach_child_subscribers(&fresh.task_id, &handle);
        let pid = handle.pid().or(fresh.pid);
        if is_terminal_record(&fresh) {
            let updated = TaskRecord {
                updated_at: self.now_iso(),
                pid,
                ..fresh.clone()
            };
            return match self.store.replace(&updated) {
                Ok(()) => ReattachResult::Ok,
                Err(error) => self.reattach_failed(&fresh.task_id, &handle, &error),
            };
        }
        let mut notification = fresh.notification.clone();
        notification.run_epoch += 1;
        let epoch = notification.run_epoch;
        let reattached = TaskRecord {
            status: TaskStatus::Running,
            updated_at: self.now_iso(),
            notification,
            pid,
            error_message: None,
            final_response: None,
            killed: None,
            ..fresh.clone()
        };
        if let Err(error) = self.store.replace(&reattached) {
            return self.reattach_failed(&fresh.task_id, &handle, &error);
        }
        lock(&self.concurrency).acquire(&reattached.model, &reattached.task_id);
        track_outcome(
            self.arc(),
            reattached.task_id.clone(),
            Arc::clone(&handle),
            reattached.model.clone(),
            epoch,
        );
        if let Err(error) = self.steering.notify_started(&reattached.task_id) {
            log(
                "senpi-task manager notify started failed",
                &reattached.task_id,
                &error,
            );
        }
        ReattachResult::Ok
    }

    fn reattach_failed(
        &self,
        task_id: &str,
        handle: &Arc<dyn ManagedChildHandle>,
        error: &dyn std::fmt::Display,
    ) -> ReattachResult {
        let detached = {
            let mut state = lock(&self.state);
            let owned = state
                .live
                .get(task_id)
                .is_some_and(|live| Arc::ptr_eq(&live.handle, handle));
            if owned {
                state.live.remove(task_id)
            } else {
                None
            }
        };
        if let Some(unsubscribe) = detached.and_then(|mut live| live.unsubscribe.take()) {
            unsubscribe();
        }
        if let Err(discard) = discard_managed_handle(handle.as_ref()) {
            log("senpi-task reattach discard failed", task_id, &discard);
        }
        log("senpi-task reattach failed", task_id, error);
        ReattachResult::Failed {
            kind: ReattachFailureKind::Failed,
            reason: "manager reattach failed".to_string(),
        }
    }
}

impl OutcomeTrackerPorts for Inner {
    fn store(&self) -> &TaskRecordStore {
        &self.store
    }

    fn now_iso(&self) -> String {
        Inner::now_iso(self)
    }

    fn live_handle(&self, task_id: &str) -> Option<Arc<dyn ManagedChildHandle>> {
        Inner::live_handle(self, task_id)
    }

    fn try_load(&self, task_id: &str) -> Option<TaskRecord> {
        Inner::try_load(self, task_id)
    }

    fn run_stats_snapshot(&self, task_id: &str) -> Option<TaskRunStats> {
        Inner::run_stats_snapshot(self, task_id)
    }

    fn release_slot(&self, task_id: &str, model: &str, epoch: i64) {
        Inner::release_slot(self, task_id, model, epoch);
    }

    fn settle_waiters(&self, task_id: &str) {
        Inner::settle_waiters(self, task_id);
    }

    fn try_runtime_fallback(&self, input: &ErrorOutcomeInput) -> bool {
        Inner::try_runtime_fallback(self, input)
    }

    fn outcome_processed(&self) {
        lock(&self.state).processed_outcomes += 1;
    }
}
