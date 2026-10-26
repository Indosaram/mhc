//! The task manager (`manager/manager.ts`). Manager state sits behind one mutex that is never held
//! across a handle, runner, store-transition or steering call, so child listeners may re-enter.

pub type FailedTurnsState = HashMap<String, Arc<Mutex<(Option<String>, Vec<String>)>>>;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use serde_json::json;

use crate::isolation::IsolationPreparation;
use crate::lifecycle::admission_lease::{
    AcquireAdmissionLeaseResult, AdmissionLeaseTimingOverrides, SessionAdmissionLease,
    acquire_session_admission_lease,
};
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
use crate::manager::isolation_wiring::{
    IsolationWiring, IsolationWiringPorts, create_isolation_wiring,
};
use crate::manager::launch_fence::{launch_run_of, owns_run};
use crate::manager::names::NameRegistry;
use crate::manager::outcome::{ErrorOutcomeInput, OutcomeTrackerPorts, track_outcome};
use crate::manager::residency_signal::{ResidencyChange, ResidencySignal};
use crate::manager::respawn::{RespawnInput, respawn_managed_task};
use crate::manager::transcript_log::subscribe_transcript_log;
use crate::manager::types::{
    Clock, FallibleAdmit, ListScope, ListedTask, ManagedRunner, ManagedRunnerError,
    ManagedStartSpec, ManagerStartSpec, NoopDestruction, OwnedStartResult, PlanResolutionCode,
    PlanResolutionError, ResidencyDeniedCause, ResolvedChildPlan, RpcRespawnRunner, SpawnAdmission,
    StartFailure, StartResult, StartedTask, TaskManagerOptions,
};
use crate::run_stats::{RunStatsTracker, create_run_stats_tracker};
use crate::runners::RunnerFailureKind;
use crate::runners::types::RpcRunnerSpec;
use crate::shared::DagTaskOwner;
use crate::state::{
    IsolationRecord, ResidencyState, SpawnSpecV1, TaskRecord, TaskRunStats, TaskSpawnSpec,
    TaskStatus, TaskTransition, create_task_record, parse_task_id, resolved_reasoning_fields,
    sync_task_id_floor,
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

/// A prepared sandbox carried from the start path to the bind (TS PreparedIsolation).
type PreparedIsolation = (
    isolation_core::IsolationHandle,
    isolation_core::WorktreeBaseline,
    crate::state::TaskIsolationSpec,
);

struct AdmissionScope(crate::manager::start_admission::StartAdmission);
impl Drop for AdmissionScope {
    fn drop(&mut self) {
        if !self.0.transferred() {
            self.0.leave();
        }
    }
}

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
    /// Tasks whose child start is in flight: the child may already run before it reaches `live`.
    launching: HashSet<String>,
    child_subscribers: HashMap<String, Vec<ChildSubscriber>>,
    released: HashMap<String, i64>,
    waiters: HashMap<String, Vec<(u64, WaitWaker)>>,
    background: HashSet<String>,
    // PERF-DIVERGENCE(senpi manager/manager.ts): upstream looks the tracker up in manager state per child event; we share the tracker through an Arc so the event closure locks only the tracker, not the manager-wide state mutex. Stats are identical.
    run_stats: HashMap<String, Arc<Mutex<Tracker>>>,
    resume_attempts: HashMap<String, (u64, Sender<()>)>,
    next_resume_attempt: u64,
    initial_turns: HashMap<String, crate::manager::child_handle::ExactTurnOutcome>,
    started_contexts: HashMap<String, LaunchContext>,
    failed_turns: FailedTurnsState,
    resume_receipts: HashMap<(String, i64, String), bool>,
    tracking_generations: HashMap<String, u64>,
    queued_launches: HashMap<String, LaunchContext>,
    held_admissions: HashMap<String, crate::manager::concurrency::QueuedTaskAdmission>,
    held_launches: HashMap<String, LaunchContext>,
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

/// What wakes a blocked [`TaskManager::wait_for`].
enum WaitWake {
    Settled(Box<TaskRecord>),
    Aborted,
}

/// Where a settle/abort wake is delivered: a blocking `wait_for` thread or an async waiter.
// PERF-DIVERGENCE(senpi manager/manager.ts): upstream parks one thread per waited task; the async variant lets workpool workers await a settle on the runtime without holding a blocking-pool thread. Wake ordering is unchanged.
#[derive(Clone)]
enum WaitWaker {
    Blocking(Sender<WaitWake>),
    Async(tokio::sync::mpsc::UnboundedSender<WaitWake>),
}

impl WaitWaker {
    /// Returns true when the receiver is gone.
    fn send(&self, wake: WaitWake) -> bool {
        match self {
            Self::Blocking(sender) => sender.send(wake).is_err(),
            Self::Async(sender) => sender.send(wake).is_err(),
        }
    }
}

/// Removes an async waiter registration when the awaiting future finishes or is dropped.
struct AsyncWaiterGuard {
    inner: Arc<Inner>,
    task_id: String,
    waiter_id: u64,
}

impl Drop for AsyncWaiterGuard {
    fn drop(&mut self) {
        self.inner.remove_waiter(&self.task_id, self.waiter_id);
    }
}

#[derive(Default)]
struct AbortState {
    aborted: bool,
    next_waker_id: u64,
    wakers: Vec<(u64, WaitWaker)>,
}

/// A cooperative abort for [`TaskManager::wait_for`] (TS `AbortSignal`).
#[derive(Clone, Default)]
pub struct AbortSignal(Arc<Mutex<AbortState>>);

impl AbortSignal {
    pub fn abort(&self) {
        let wakers = {
            let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            state.aborted = true;
            std::mem::take(&mut state.wakers)
        };
        for (_, waker) in wakers {
            let _receiver_gone = waker.send(WaitWake::Aborted);
        }
    }

    pub fn aborted(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .aborted
    }

    /// Registers a waker pinged by [`Self::abort`]; an already-aborted signal pings at once.
    fn register(&self, waker: WaitWaker) -> u64 {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.next_waker_id += 1;
        let id = state.next_waker_id;
        if state.aborted {
            let _receiver_gone = waker.send(WaitWake::Aborted);
        } else {
            state.wakers.push((id, waker));
        }
        id
    }

    fn unregister(&self, id: u64) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .wakers
            .retain(|(waker_id, _)| *waker_id != id);
    }
}

/// Drops an [`AbortSignal`] waker registration when `wait_for` returns.
struct AbortWakerGuard<'a>(&'a AbortSignal, u64);

impl Drop for AbortWakerGuard<'_> {
    fn drop(&mut self) {
        self.0.unregister(self.1);
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
    fallible_admit: Option<FallibleAdmit>,
    traceparent_for: Option<crate::manager::types::TraceparentFor>,
    admission_barrier: crate::pause::admission_barrier::AdmissionBarrier,
    child_pause: Arc<dyn crate::pause::ports::ChildPausePort>,
    held_callback: Option<crate::manager::start_admission::HeldCallback>,
    host_transport: Option<Arc<dyn crate::lifecycle::HostTransport>>,
    host_status_publication: Mutex<()>,
    launches_changed: std::sync::Condvar,
    trusted_respawn_launch: Option<crate::manager::types::TrustedRespawnLaunchResolver>,
    host_pid: i64,
    rpc_respawn_runner: Arc<dyn RpcRespawnRunner>,
    names: Mutex<NameRegistry>,
    concurrency: Mutex<TaskConcurrency>,
    state: Mutex<State>,
    steering: SteeringEngine,
    isolation: IsolationWiring,
    pauses_pending: PendingStops,
    resume_capacity: Mutex<()>,
    parked: Mutex<HashMap<String, crate::manager::concurrency::ParkedLease>>,
    stops_pending: PendingStops,
    residency: ResidencySignal,
    this: Weak<Inner>,
}

/// Pending cancels, keyed by the EXACT run (task id + run epoch) so a signal can never be inherited
/// by a later run of the same task. The watcher takes the entry; the cancel guard retains the same
/// signal independently and drops any entry no watcher will consume.
#[derive(Default)]
struct PendingStops {
    pending: Mutex<HashMap<(String, i64), crate::manager::outcome::PendingStop>>,
}

impl PendingStops {
    fn request(&self, task_id: &str, epoch: i64) -> crate::manager::outcome::PendingStop {
        Arc::clone(
            lock(&self.pending)
                .entry((task_id.to_string(), epoch))
                .or_insert_with(|| Arc::new((Mutex::new(false), std::sync::Condvar::new()))),
        )
    }

    fn take_settlement(
        &self,
        task_id: &str,
        epoch: i64,
    ) -> Option<crate::manager::outcome::PendingStop> {
        lock(&self.pending).remove(&(task_id.to_string(), epoch))
    }

    /// Marks the pending stop for this exact run settled and wakes its outcome watcher.
    fn settle(&self, task_id: &str, epoch: i64) {
        let pending = lock(&self.pending)
            .get(&(task_id.to_string(), epoch))
            .cloned();
        if let Some(pending) = pending {
            *lock(&pending.0) = true;
            pending.1.notify_all();
        }
    }

    fn forget(&self, task_id: &str, epoch: i64) {
        let _ = lock(&self.pending).remove(&(task_id.to_string(), epoch));
    }
}

/// Settles a pending stop on every exit path of a cancel (an early `?` and a panic included) so the
/// outcome watcher is always notified, and drops the entry when this cancel leaves no watcher to
/// consume it.
struct StopSettlementGuard<'a> {
    stops: &'a PendingStops,
    task_id: String,
    epoch: i64,
    pending: crate::manager::outcome::PendingStop,
    consumed: bool,
    /// A cancel left pending on an unreachable child settles through the port's `stop_settled`, not here.
    deferred: bool,
}

impl Drop for StopSettlementGuard<'_> {
    fn drop(&mut self) {
        if self.deferred {
            return;
        }
        *lock(&self.pending.0) = true;
        self.pending.1.notify_all();
        if !self.consumed {
            self.stops.forget(&self.task_id, self.epoch);
        }
    }
}

/// Runs a queue grant on its own thread. Under test the in-flight grant is counted so the chaos
/// harness can wait for the exact launch it triggered instead of racing the scheduler.
fn spawn_grant(grant: Grant) {
    #[cfg(test)]
    crate::manager::outcome::test_barrier::grant_spawned();
    std::thread::spawn(move || {
        #[cfg(test)]
        let _done = crate::manager::outcome::test_barrier::GrantDone;
        grant();
    });
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
        let isolation = create_isolation_wiring(IsolationWiringPorts {
            runtime: options.isolation,
            store: store.clone(),
            cwd: options.cwd.clone(),
            host_pid: options
                .host_pid
                .unwrap_or_else(|| i64::from(std::process::id())),
            settings: options.isolation_settings,
        });
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
                traceparent_for: options.traceparent_for,
                held_callback: options.on_held_admission,
                host_transport: options.host_transport,
                host_status_publication: Mutex::new(()),
                launches_changed: std::sync::Condvar::new(),
                admission_barrier: options.admission_barrier.unwrap_or_default(),
                child_pause: options.child_pause_port.unwrap_or_else(|| {
                    Arc::new(crate::pause::native_child_pause::NativeChildPausePort)
                }),
                trusted_respawn_launch: options.trusted_respawn_launch,
                host_pid: options
                    .host_pid
                    .unwrap_or_else(|| i64::from(std::process::id())),
                rpc_respawn_runner,
                names: Mutex::new(NameRegistry::default()),
                state: Mutex::new(State::default()),
                steering: SteeringEngine::new(port),
                isolation,
                pauses_pending: PendingStops::default(),
                resume_capacity: Mutex::new(()),
                parked: Mutex::default(),
                stops_pending: PendingStops::default(),
                residency: ResidencySignal::default(),
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

    pub fn admission_barrier(&self) -> &crate::pause::admission_barrier::AdmissionBarrier {
        &self.inner.admission_barrier
    }

    pub fn traceparent_for(&self, parent_session_id: &str) -> Option<String> {
        self.inner
            .traceparent_for
            .as_ref()
            .and_then(|resolve| resolve(parent_session_id))
    }

    pub fn store(&self) -> &TaskRecordStore {
        &self.inner.store
    }

    pub fn residency_changed(&self, parent_session_id: &str) -> ResidencyChange {
        self.inner.residency.changed(parent_session_id)
    }

    pub fn has_free_slot(&self, model: &str) -> bool {
        lock(&self.inner.concurrency).has_free_slot(model)
    }
    pub fn state_dir(&self) -> &Path {
        self.inner.store.state_dir()
    }
    pub fn has_pending_sends(&self, task_id: &str) -> bool {
        self.inner.steering.has_pending_sends(task_id)
    }
    pub fn has_in_flight_sends(&self, task_id: &str) -> bool {
        self.inner.steering.has_in_flight_send(task_id)
    }
    pub fn try_begin_send(&self, task_id: &str) -> bool {
        self.inner.steering.try_begin_send(task_id)
    }
    pub fn end_send(&self, task_id: &str) {
        self.inner.steering.end_send(task_id);
    }
    pub fn run_epoch(&self, task_id: &str) -> Option<i64> {
        self.inner
            .try_load(task_id)
            .map(|record| record.notification.run_epoch)
    }
    pub fn next_workpool_epoch(&self, task_id: &str) -> Option<i64> {
        self.run_epoch(task_id).map(|epoch| epoch.saturating_add(1))
    }

    pub fn workpool_worker_eligible(
        &self,
        task_id: &str,
        parent_session_id: &str,
        expected_epoch: i64,
    ) -> bool {
        let Some(record) = self.inner.try_load(task_id) else {
            return false;
        };
        record.parent_session_id == parent_session_id
            && record.killed != Some(true)
            && record.notification.run_epoch.checked_add(1) == Some(expected_epoch)
            && crate::state::messageability(record.status, record.residency_state)
                == crate::state::Messageability::Revive
            && self.inner.live_handle(task_id).is_some()
            && record.pending_steering.as_ref().is_none_or(Vec::is_empty)
    }

    pub fn workpool_worker_record(&self, task_id: &str) -> Option<TaskRecord> {
        self.inner.try_load(task_id)
    }

    pub fn reserve_slot(&self, model: &str, task_id: &str, epoch: i64) -> bool {
        let mut concurrency = lock(&self.inner.concurrency);
        if !concurrency.has_free_slot(model) {
            return false;
        }
        if !concurrency.try_acquire(model, task_id, epoch) {
            return false;
        }
        lock(&self.inner.state)
            .released
            .insert(task_id.to_string(), epoch.saturating_sub(1));
        true
    }

    pub fn allocate_workpool_record(
        &self,
        spec: &crate::workpool::types::WorkpoolSpec,
        task_id: &str,
        host_pid: i64,
    ) -> Result<(TaskRecord, ManagedStartSpec), crate::state::TaskIdSpaceExhaustedError> {
        let mode = spec.start.execution_mode.unwrap_or_default();
        let input =
            crate::manager::helpers::build_record_input(&spec.start, &spec.plan, task_id, mode);
        let mut record = create_task_record(input, u64::try_from((self.inner.now)()).ok())?;
        record.task_id = task_id.to_string();
        record.host_pid = Some(host_pid);
        record.notify_on_terminal = false;
        let managed = build_managed_spec(
            &record,
            &spec.start,
            &spec.plan,
            &self.inner.cwd,
            self.inner.store.state_dir(),
            self.traceparent_for(&spec.start.parent_session_id),
        );
        record.spawn_spec = Some(TaskSpawnSpec::V1(build_spawn_spec_v1(&managed)));
        Ok((record, managed))
    }

    pub fn enqueue_workpool(
        &self,
        model: &str,
        task_id: &str,
        epoch: i64,
        grant: crate::manager::concurrency::Grant,
    ) -> (usize, bool) {
        let mut grant = Some(grant);
        let (position, immediate) = {
            let mut concurrency = lock(&self.inner.concurrency);
            lock(&self.inner.state)
                .released
                .insert(task_id.to_string(), epoch.saturating_sub(1));
            if concurrency.has_free_slot(model) {
                let acquired = concurrency.try_acquire(model, task_id, epoch);
                debug_assert!(acquired);
                (0, true)
            } else {
                (
                    concurrency.enqueue_epoch(
                        model,
                        task_id,
                        epoch,
                        grant.take().expect("pending grant"),
                    ),
                    false,
                )
            }
        };
        if immediate {
            spawn_grant(grant.expect("immediate grant"));
        }
        (position, immediate)
    }

    pub fn remove_workpool_waiter(&self, model: &str, task_id: &str) -> bool {
        lock(&self.inner.concurrency).remove(model, task_id)
    }

    pub fn release_reserved_slot(&self, task_id: &str, model: &str, epoch: i64) {
        self.inner.release_slot(task_id, model, epoch);
    }

    pub fn resolve_workpool_spec(
        &self,
        caller: &crate::workpool::types::WorkpoolCaller,
        agent: &crate::workpool::types::WorkpoolAgent,
    ) -> Result<crate::workpool::types::WorkpoolSpec, crate::workpool::ports::WorkpoolAdmissionError>
    {
        let mut spec = match agent {
            crate::workpool::types::WorkpoolAgent::Category {
                prompt,
                model,
                category,
            } => ManagerStartSpec {
                prompt: prompt.clone(),
                model: model.clone(),
                category: Some(category.clone()),
                ..ManagerStartSpec::default()
            },
            crate::workpool::types::WorkpoolAgent::SubagentType {
                prompt,
                model,
                subagent_type,
            } => ManagerStartSpec {
                prompt: prompt.clone(),
                model: model.clone(),
                subagent_type: Some(subagent_type.clone()),
                ..ManagerStartSpec::default()
            },
        };
        spec.parent_session_id = caller.session_id.clone();
        spec.root_session_id = Some(caller.root_session_id.clone());
        spec.depth = caller.depth.saturating_add(1);
        spec.cwd = Some(caller.cwd.clone());
        let plan = (self.inner.planner)(&spec).map_err(|error| {
            crate::workpool::ports::WorkpoolAdmissionError(error.message.clone())
        })?;
        let target = spec.subagent_type.as_deref().or(plan.agent_type.as_deref());
        let allowed = plan.allowed_subagents.as_deref().unwrap_or_default();
        if let crate::manager::depth_policy::DepthDecision::Denied { reason } =
            crate::manager::depth_policy::decide_depth_policy(
                &crate::manager::depth_policy::DepthPolicyInput {
                    child_depth: spec.depth,
                    max_depth: plan.max_depth.unwrap_or(self.inner.config.max_depth),
                    target_agent_type: target,
                    allowed_subagents: Some(allowed),
                },
            )
        {
            return Err(crate::workpool::ports::WorkpoolAdmissionError(reason));
        }
        spec.execution_mode = Some(crate::manager::execution_mode::resolve_execution_mode(
            crate::manager::execution_mode::ExecutionModeSources {
                spec_mode: None,
                agent_mode: plan.agent_execution_mode,
                config_mode: Some(self.inner.config.default_execution_mode),
            },
        ));
        Ok(crate::workpool::types::WorkpoolSpec { start: spec, plan })
    }

    pub fn admission_authorize(
        &self,
        session_id: &str,
    ) -> Result<(), crate::workpool::ports::WorkpoolAdmissionError> {
        let admission = if let Some(admit) = &self.inner.fallible_admit {
            admit(session_id).map_err(|error| {
                crate::workpool::ports::WorkpoolAdmissionError(error.to_string())
            })?
        } else if let Some(admit) = &self.inner.admit {
            admit(session_id)
        } else {
            return Ok(());
        };
        match admission {
            SpawnAdmission::Rejected { message, .. } => {
                Err(crate::workpool::ports::WorkpoolAdmissionError(message))
            }
            SpawnAdmission::Admitted | SpawnAdmission::Evicted { .. } => Ok(()),
        }
    }

    pub fn launch_prepared(
        &self,
        record: TaskRecord,
        managed_spec: ManagedStartSpec,
    ) -> Result<(), String> {
        let mode = ExecutionMode::parse(&record.execution_mode)
            .ok_or_else(|| "Invalid worker execution mode".to_owned())?;
        let context = LaunchContext {
            runner: Arc::clone(self.inner.runners.get(mode)),
            model: record.model.clone(),
            managed_spec,
            record,
        };
        self.inner.launch(&context)
    }

    pub fn revive_workpool_task(
        &self,
        task_id: &str,
        epoch: i64,
        message: &str,
    ) -> Result<SendOutcome, SteeringError> {
        let record = self.inner.try_load(task_id).ok_or_else(|| {
            SteeringError::Store(crate::store::StoreError::NotFound(task_id.to_string()))
        })?;
        if record.notification.run_epoch.checked_add(1) != Some(epoch)
            || crate::state::messageability(record.status, record.residency_state)
                != crate::state::Messageability::Revive
        {
            return Ok(SendOutcome::NotContinuable {
                task_id: task_id.to_string(),
                reason: "Worker run epoch changed before revival.".into(),
                suggestion: "Inspect the task before retrying.".into(),
            });
        }
        let handle = self.inner.live_handle(task_id).ok_or_else(|| {
            SteeringError::Host(crate::host::HostError {
                message: "worker has no resident session for revival".into(),
            })
        })?;
        let identity = crate::manager::child_handle::WorkpoolChildIdentity {
            task_id: task_id.to_string(),
            state_dir: self.inner.store.state_dir().to_string_lossy().into_owned(),
            run_epoch: u32::try_from(epoch).map_err(|error| {
                SteeringError::Host(crate::host::HostError {
                    message: error.to_string(),
                })
            })?,
        };
        let mut revived = record.clone();
        revived.final_response = None;
        revived.error_message = None;
        revived.run_stats = None;
        revived.status = TaskStatus::Running;
        revived.residency_state = ResidencyState::Resident;
        revived.updated_at = self.inner.now_iso();
        revived.notification.run_epoch = epoch;
        let mut pending = revived.clone();
        pending.status = TaskStatus::Pending;
        self.inner
            .store
            .replace(&pending)
            .map_err(SteeringError::Store)?;
        let rebind = handle.rebind_workpool_identity(identity);
        match rebind {
            Some(Ok(())) => {}
            Some(Err(_error)) => {
                self.inner
                    .store
                    .replace(&revived)
                    .map_err(SteeringError::Store)?;
                self.inner.settle_failed_workpool_revival(
                    task_id,
                    epoch,
                    "Resident worker refused workpool identity rebinding.",
                )?;
                return Ok(SendOutcome::NotContinuable {
                    task_id: task_id.to_string(),
                    reason: "Resident worker refused workpool identity rebinding.".into(),
                    suggestion: "Start a fresh worker instead of reusing this session.".into(),
                });
            }
            None => {
                self.inner
                    .store
                    .replace(&revived)
                    .map_err(SteeringError::Store)?;
                self.inner.settle_failed_workpool_revival(
                    task_id,
                    epoch,
                    "Resident worker does not support workpool identity rebinding.",
                )?;
                return Ok(SendOutcome::NotContinuable {
                    task_id: task_id.to_string(),
                    reason: "Resident worker does not support workpool identity rebinding.".into(),
                    suggestion: "Start a fresh worker instead of reusing this session.".into(),
                });
            }
        }
        if let Err(error) = handle.follow_up(message) {
            self.inner
                .store
                .replace(&revived)
                .map_err(SteeringError::Store)?;
            self.inner.settle_failed_workpool_revival(
                task_id,
                epoch,
                "Workpool turn delivery is uncertain after identity rebinding.",
            )?;
            self.inner
                .store
                .append_event(
                    task_id,
                    &PersistedTaskEvent {
                        event_type: "workpool_revival_delivery_uncertain".into(),
                        payload: serde_json::json!({"run_epoch":epoch,"error":error.to_string()}),
                    },
                )
                .map_err(SteeringError::Store)?;
            return Ok(SendOutcome::NotContinuable {
                task_id: task_id.to_string(),
                reason: "Workpool turn delivery is uncertain after identity rebinding.".into(),
                suggestion: "Do not retry this assignment; start a fresh worker.".into(),
            });
        }
        let mut running = self.inner.try_load(task_id).ok_or_else(|| {
            SteeringError::Store(crate::store::StoreError::NotFound(task_id.to_string()))
        })?;
        if running.notification.run_epoch != epoch {
            return Ok(SendOutcome::NotContinuable {
                task_id: task_id.to_string(),
                reason: "Worker run epoch changed during revival.".into(),
                suggestion: "Inspect the task before retrying.".into(),
            });
        }
        running.status = TaskStatus::Running;
        running.residency_state = ResidencyState::Resident;
        running.updated_at = self.inner.now_iso();
        self.inner
            .store
            .replace(&running)
            .map_err(SteeringError::Store)?;
        let _ = self.inner.store.append_event(
            task_id,
            &PersistedTaskEvent {
                event_type: "task_revived".into(),
                payload: serde_json::json!({"run_epoch":epoch}),
            },
        );
        self.inner.reacquire_for_revive(task_id, epoch);
        Ok(SendOutcome::Revived {
            task_id: task_id.to_string(),
            run_epoch: epoch,
        })
    }

    pub fn start(&self, spec: &ManagerStartSpec) -> StartResult {
        if let Some(refusal) = member_kernel_tool_refusal(spec) {
            return refusal;
        }
        self.start_prepared(spec, |_, _, _| Ok(()))
    }

    pub fn start_prepared(
        &self,
        spec: &ManagerStartSpec,
        prepare: impl FnOnce(
            &mut ManagerStartSpec,
            &ResolvedChildPlan,
            ExecutionMode,
        ) -> Result<(), String>,
    ) -> StartResult {
        if let Some(refusal) = member_kernel_tool_refusal(spec) {
            return refusal;
        }
        let entered = crate::manager::start_admission::enter_start_admission(
            crate::manager::start_admission::EnterStartAdmissionInput {
                barrier: &self.inner.admission_barrier,
                spec,
                task_id: spec.name.clone().unwrap_or_else(|| "start".into()),
                run_ref: None,
                credential: spec.resume_credential.clone(),
                held: self.inner.held_callback.clone(),
            },
        );
        let entered = match entered {
            crate::manager::start_admission::StartAdmissionEntry::Entered(entered) => entered,
            crate::manager::start_admission::StartAdmissionEntry::Held(held) => {
                return StartResult::Held(held);
            }
        };
        let _scope = AdmissionScope(entered.scope);
        let spec = &entered.spec;
        let plan = match (self.inner.planner)(spec) {
            Ok(plan) => plan,
            Err(error) => return StartResult::PlanUnresolved(*error),
        };
        let (admission_lease, rejection) = self.admission_rejection(spec, &plan);
        if let Some(rejected) = rejection {
            if let Some(lease) = admission_lease {
                lease.release();
            }
            return rejected;
        }
        let mut spec = spec.clone();
        let mode = resolve_execution_mode(ExecutionModeSources {
            spec_mode: spec.execution_mode,
            agent_mode: plan.agent_execution_mode,
            config_mode: Some(self.inner.config.default_execution_mode),
        });
        if let Err(message) = prepare(&mut spec, &plan, mode) {
            if let Some(lease) = admission_lease {
                lease.release();
            }
            return StartResult::PlanUnresolved(PlanResolutionError::new(
                PlanResolutionCode::InvalidTarget,
                message,
            ));
        }
        self.inner
            .start_resolved(&spec, &plan, None, admission_lease)
    }

    pub fn start_owned(&self, spec: &ManagerStartSpec, owner: &DagTaskOwner) -> OwnedStartResult {
        let entered = crate::manager::start_admission::enter_start_admission(
            crate::manager::start_admission::EnterStartAdmissionInput {
                barrier: &self.inner.admission_barrier,
                spec,
                task_id: owner.node_id.clone(),
                run_ref: None,
                credential: spec.resume_credential.clone(),
                held: self.inner.held_callback.clone(),
            },
        );
        let entered = match entered {
            crate::manager::start_admission::StartAdmissionEntry::Entered(entered) => entered,
            crate::manager::start_admission::StartAdmissionEntry::Held(held) => {
                return OwnedStartResult::NotStarted(StartResult::Held(held));
            }
        };
        let _scope = AdmissionScope(entered.scope);
        let spec = &entered.spec;
        let plan = match (self.inner.planner)(spec) {
            Ok(plan) => plan,
            Err(error) => return OwnedStartResult::NotStarted(StartResult::PlanUnresolved(*error)),
        };
        let (admission_lease, rejection) = self.admission_rejection(spec, &plan);
        if let Some(rejected) = rejection {
            if let Some(lease) = admission_lease {
                lease.release();
            }
            return OwnedStartResult::NotStarted(rejected);
        }
        let lock_path = match owner_lock_path(self.inner.store.state_dir(), owner) {
            Ok(path) => path,
            Err(error) => {
                if let Some(lease) = admission_lease {
                    lease.release();
                }
                return OwnedStartResult::NotStarted(bookkeeping_failure(spec, &plan, error));
            }
        };
        let locked = with_task_record_lock(&lock_path, || {
            if let Some(raced) = self.owned_result(owner) {
                return Ok(raced);
            }
            Ok(
                match self
                    .inner
                    .start_resolved(spec, &plan, Some(owner), admission_lease.clone())
                {
                    StartResult::Started(task) => OwnedStartResult::Started {
                        task,
                        reused: false,
                    },
                    other => OwnedStartResult::NotStarted(other),
                },
            )
        });
        match locked {
            Ok(result) => result,
            Err(error) => {
                if let Some(lease) = &admission_lease {
                    lease.release();
                }
                OwnedStartResult::NotStarted(bookkeeping_failure(spec, &plan, error))
            }
        }
    }

    fn admission_rejection(
        &self,
        spec: &ManagerStartSpec,
        plan: &ResolvedChildPlan,
    ) -> (Option<Arc<dyn SessionAdmissionLease>>, Option<StartResult>) {
        if self.inner.admit.is_none() && self.inner.fallible_admit.is_none() {
            return (None, None);
        }
        if let Some(admission) = &spec.admission
            && let Err(held) = admission.assert_open() {
                return (None, Some(StartResult::Held(held.result)));
            }
        let lease = match acquire_session_admission_lease(
            self.inner.store.state_dir(),
            &spec.parent_session_id,
            AdmissionLeaseTimingOverrides::default(),
        ) {
            Ok(AcquireAdmissionLeaseResult::Acquired(lease)) => lease,
            Ok(AcquireAdmissionLeaseResult::Contended) => {
                return (
                    None,
                    Some(StartResult::ResidencyDenied {
                        reason: "Residency admission is contended.".to_string(),
                        cause: ResidencyDeniedCause::Lease,
                    }),
                );
            }
            Err(error) => {
                return (
                    None,
                    Some(StartResult::StartFailed(StartFailure {
                        task_id: String::new(),
                        name: spec.name.clone().unwrap_or_default(),
                        category: spec.category.clone().or(plan.category.clone()),
                        subagent_type: spec.subagent_type.clone().or(plan.agent_type.clone()),
                        execution_mode: spec.execution_mode.unwrap_or_default(),
                        model: plan.model.clone(),
                        resolved_model: plan.resolved_model.clone(),
                        run_in_background: spec.run_in_background,
                        error_message: error.to_string(),
                    })),
                );
            }
        };
        let admission = if let Some(admit) = &self.inner.fallible_admit {
            match admit(&spec.parent_session_id) {
                Ok(admission) => admission,
                Err(error) => {
                    lease.release();
                    return (
                        None,
                        Some(StartResult::StartFailed(StartFailure {
                            task_id: String::new(),
                            name: spec.name.clone().unwrap_or_default(),
                            category: spec.category.clone().or(plan.category.clone()),
                            subagent_type: spec.subagent_type.clone().or(plan.agent_type.clone()),
                            execution_mode: spec.execution_mode.unwrap_or_default(),
                            model: plan.model.clone(),
                            resolved_model: plan.resolved_model.clone(),
                            run_in_background: spec.run_in_background,
                            error_message: error.to_string(),
                        })),
                    );
                }
            }
        } else if let Some(admit) = &self.inner.admit {
            admit(&spec.parent_session_id)
        } else {
            return (Some(lease), None);
        };
        if let Some(scope) = &spec.admission
            && let Err(held) = scope.assert_open() {
                lease.release();
                return (None, Some(StartResult::Held(held.result)));
            }
        match admission {
            SpawnAdmission::Rejected {
                message,
                max_children,
                residents,
            } => {
                let residents = residents.unwrap_or_default();
                let cause = ResidencyDeniedCause::Residents {
                    max_children,
                    residents,
                };
                lease.release();
                (
                    None,
                    Some(StartResult::ResidencyDenied {
                        reason: message,
                        cause,
                    }),
                )
            }
            SpawnAdmission::Admitted | SpawnAdmission::Evicted { .. } => {
                if !lease.is_owner() {
                    lease.release();
                    return (
                        None,
                        Some(StartResult::ResidencyDenied {
                            reason: "Residency admission lease was displaced.".to_string(),
                            cause: ResidencyDeniedCause::Lease,
                        }),
                    );
                }
                (Some(lease), None)
            }
        }
    }

    pub fn find_owned_task(&self, run_id: &str, node_id: &str) -> Option<TaskRecord> {
        self.inner
            .store
            .find_record(|record| {
                record
                    .owner
                    .as_ref()
                    .is_some_and(|owner| owner.run_id == run_id && owner.node_id == node_id)
            })
            .ok()?
    }

    fn owned_result(&self, owner: &DagTaskOwner) -> Option<OwnedStartResult> {
        let record = self.find_owned_task(&owner.run_id, &owner.node_id)?;
        let existing = record
            .owner
            .as_ref()
            .map(|owner| owner.fingerprint.clone())
            .unwrap_or_default();
        if existing != owner.fingerprint {
            // A settled record cannot be re-run in place, so a differing fingerprint on it is a
            // retry: ownership moves to the fresh task. Only a live task is a genuine conflict.
            if crate::manager::helpers::is_terminal_record(&record) {
                return None;
            }
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

    pub fn withdraw_admissions(&self, root_session_id: &str) {
        let contexts: Vec<_> = lock(&self.inner.state)
            .queued_launches
            .values()
            .filter(|c| c.record.root_session_id == root_session_id)
            .cloned()
            .collect();
        for context in contexts {
            let admission = lock(&self.inner.concurrency).withdraw(
                &context.model,
                &context.record.task_id,
                context.record.notification.run_epoch,
            );
            if let Some(admission) = admission {
                if let Err(error) =
                    self.inner
                        .store
                        .mutate(&context.record.task_id, |record: &TaskRecord| {
                            TaskRecord {
                                queued_admission: Some(admission.clone()),
                                ..record.clone()
                            }
                        })
                {
                    utils::logger::log(
                        "senpi-task failed to persist held admission",
                        Some(&json!({"taskId":context.record.task_id,"error":error.to_string()})),
                    );
                }
                let mut state = lock(&self.inner.state);
                state
                    .held_admissions
                    .insert(context.record.task_id.clone(), admission);
                state.queued_launches.remove(&context.record.task_id);
                state
                    .held_launches
                    .insert(context.record.task_id.clone(), context);
            }
        }
    }

    pub fn resume_held_task(
        &self,
        task_id: &str,
        credential: &crate::pause::admission_barrier::ResumeCredential,
    ) -> Result<(), String> {
        self.resume_held_task_with_allocation(task_id, credential, None)
    }

    pub fn resume_held_task_with_allocation(
        &self,
        task_id: &str,
        credential: &crate::pause::admission_barrier::ResumeCredential,
        saved: Option<&crate::manager::concurrency::QueuedTaskAdmission>,
    ) -> Result<(), String> {
        let record = self
            .inner
            .try_load(task_id)
            .ok_or("held admission record_missing")?;
        if record.status.is_terminal() {
            return Ok(());
        }
        if record.status != TaskStatus::Pending {
            return Err("held admission already_started".into());
        }
        if record.host_pid.is_some_and(|pid| {
            pid != self.inner.host_pid
                && u32::try_from(pid).is_ok_and(utils::process_sweep::default_is_process_alive)
        }) {
            return Err("held admission owner is still alive".into());
        }
        let admission = saved
            .cloned()
            .or_else(|| {
                lock(&self.inner.state)
                    .held_admissions
                    .get(task_id)
                    .cloned()
            })
            .or_else(|| record.queued_admission.clone())
            .ok_or("held admission allocation_missing")?;
        if admission.task_id != task_id {
            return Err("held admission task_mismatch".into());
        }
        if admission.run_epoch != record.notification.run_epoch {
            return Err("held admission stale_epoch".into());
        }
        let warm = lock(&self.inner.state).held_launches.get(task_id).cloned();
        let mut managed_spec = match &warm {
            Some(context) => context.managed_spec.clone(),
            None => crate::manager::helpers::build_respawn_managed_spec(
                &record,
                self.inner.store.state_dir(),
            )
            .map_err(str::to_string)?,
        };
        let request = ManagerStartSpec {
            prompt: managed_spec.prompt.clone(),
            parent_session_id: record.parent_session_id.clone(),
            root_session_id: Some(record.root_session_id.clone()),
            depth: record.depth,
            ..Default::default()
        };
        let entered = crate::manager::start_admission::enter_start_admission(
            crate::manager::start_admission::EnterStartAdmissionInput {
                barrier: &self.inner.admission_barrier,
                spec: &request,
                task_id: task_id.into(),
                run_ref: None,
                credential: Some(credential.clone()),
                held: None,
            },
        );
        let scope = match entered {
            crate::manager::start_admission::StartAdmissionEntry::Entered(entered) => entered.scope,
            crate::manager::start_admission::StartAdmissionEntry::Held(_) => {
                return Err("run admission held".into());
            }
        };
        let guard = AdmissionScope(scope.clone());
        let mut claimed = false;
        let owned = self
            .inner
            .store
            .mutate(task_id, &mut |fresh: &TaskRecord| {
                if fresh.status != TaskStatus::Pending
                    || fresh.notification.run_epoch != record.notification.run_epoch
                    || fresh.host_pid != record.host_pid
                {
                    return fresh.clone();
                }
                claimed = true;
                TaskRecord {
                    host_pid: Some(self.inner.host_pid),
                    queued_admission: None,
                    ..fresh.clone()
                }
            })
            .map_err(|error| error.to_string())?
            .ok_or("held admission record_missing")?;
        if !claimed {
            return Err("held admission owner_changed".into());
        }
        managed_spec.admission = Some(scope.clone());
        let mode = if owned.execution_mode == "in-process" {
            ExecutionMode::InProcess
        } else {
            ExecutionMode::Process
        };
        let context = LaunchContext {
            runner: warm.map_or_else(
                || Arc::clone(self.inner.runners.get(mode)),
                |warm| warm.runner,
            ),
            record: owned,
            managed_spec,
            model: admission.model.clone(),
        };
        lock(&self.inner.state)
            .held_launches
            .insert(task_id.into(), context.clone());
        let inner = Arc::clone(&self.inner);
        lock(&self.inner.concurrency).requeue(
            admission,
            Box::new(move || {
                let _ = inner.launch(&context);
            }),
        );
        scope.transfer();
        drop(guard);
        for grant in lock(&self.inner.concurrency).drain() {
            spawn_grant(grant);
        }
        Ok(())
    }

    pub fn pause_task(
        &self,
        task_id: &str,
        owner: &crate::pause::ports::RunPauseOwner,
    ) -> Result<u64, String> {
        let handle = self
            .inner
            .live_handle(task_id)
            .ok_or("task is not resident")?;
        let native = &self.inner.child_pause;
        let record = self
            .inner
            .try_load(task_id)
            .ok_or("task record unavailable")?;
        if record
            .run_pause
            .as_ref()
            .is_some_and(|p| p.parent_session_id != owner.parent_session_id)
        {
            return Err("wrong_parent".into());
        }
        self.inner
            .admission_barrier
            .close(&record.root_session_id, owner.parent_pause_seq);
        self.withdraw_admissions(&record.root_session_id);
        if let Some((_, wake)) = lock(&self.inner.state).resume_attempts.remove(task_id) {
            let _ = wake.send(());
        }
        lock(&self.inner.concurrency).withdraw(
            &record.model,
            task_id,
            record.notification.run_epoch,
        );
        if let Some(lease) = lock(&self.inner.parked).get(task_id).cloned() {
            lock(&self.inner.concurrency).suspend_unpark(&lease);
        }
        let pending = self
            .inner
            .pauses_pending
            .request(task_id, record.notification.run_epoch);
        let result = native
            .pause_for_run(handle.as_ref(), owner)
            .or_else(|error| {
                match native.observe_hold(handle.as_ref(), &owner.parent_session_id) {
                    Ok(Some(observed)) if observed.held => {
                        Ok(crate::pause::ports::EngineInheritReceipt::Inherited {
                            child_seq: observed.child_seq,
                        })
                    }
                    Ok(_) => Err(error),
                    Err(observation) => {
                        log(
                            "senpi-task pause recovery observation failed",
                            task_id,
                            &observation,
                        );
                        Err(error)
                    }
                }
            });
        match result {
            Ok(crate::pause::ports::EngineInheritReceipt::WrongParent) => {
                self.inner
                    .pauses_pending
                    .settle(task_id, record.notification.run_epoch);
                self.inner
                    .pauses_pending
                    .forget(task_id, record.notification.run_epoch);
                Err("wrong_parent".into())
            }
            Ok(crate::pause::ports::EngineInheritReceipt::Inherited { child_seq }) => {
                let Some(mut fresh) = self
                    .inner
                    .try_load(task_id)
                    .filter(|r| r.notification.run_epoch == record.notification.run_epoch)
                else {
                    self.inner
                        .pauses_pending
                        .settle(task_id, record.notification.run_epoch);
                    return Err("stale pause".into());
                };
                self.inner.next_tracking_generation(task_id);
                fresh.status = TaskStatus::Paused;
                fresh.run_pause = Some(crate::state::RecordedRunPause {
                    parent_session_id: owner.parent_session_id.clone(),
                    parent_pause_seq: owner.parent_pause_seq,
                    run_ref: crate::state::RunPauseRef {
                        id: owner.run_ref.id.clone(),
                        kind: match owner.run_ref.kind {
                            crate::pause::ports::RunRefKind::Task => {
                                crate::state::RunPauseKind::Task
                            }
                            crate::pause::ports::RunRefKind::Dag => crate::state::RunPauseKind::Dag,
                            crate::pause::ports::RunRefKind::Team => {
                                crate::state::RunPauseKind::Team
                            }
                            crate::pause::ports::RunRefKind::Workpool => {
                                crate::state::RunPauseKind::Workpool
                            }
                        },
                    },
                });
                let persisted = self.inner.store.replace(&fresh).map_err(|e| e.to_string());
                if persisted.is_ok() {
                    if let Some(lease) =
                        lock(&self.inner.concurrency).park(task_id, record.notification.run_epoch)
                    {
                        lock(&self.inner.parked).insert(task_id.into(), lease);
                    }
                    let grants = lock(&self.inner.concurrency).drain();
                    for grant in grants {
                        spawn_grant(grant);
                    }
                }
                *lock(&pending.0) = true;
                pending.1.notify_all();
                persisted?;
                Ok(child_seq)
            }
            Err(error) => {
                *lock(&pending.0) = true;
                pending.1.notify_all();
                self.inner
                    .pauses_pending
                    .forget(task_id, record.notification.run_epoch);
                Err(error.to_string())
            }
        }
    }

    pub fn resume_paused_task(
        &self,
        task_id: &str,
        owner: &crate::pause::ports::RunPauseOwner,
        resume_id: &str,
        child_seq_cutoff: u64,
    ) -> Result<bool, String> {
        let handle = self
            .inner
            .live_handle(task_id)
            .ok_or("task is not resident")?;
        let native = &self.inner.child_pause;
        let record = self
            .inner
            .try_load(task_id)
            .ok_or("task record unavailable")?;
        if let Some(receipt) = lock(&self.inner.state)
            .resume_receipts
            .get(&(
                task_id.into(),
                record.notification.run_epoch,
                resume_id.into(),
            ))
            .copied()
        {
            return Ok(receipt);
        }
        let credential = self
            .inner
            .admission_barrier
            .resume(&record.root_session_id, owner.parent_pause_seq, resume_id)
            .map_err(|e| e.to_string())?;
        let held_context = lock(&self.inner.state).held_launches.remove(task_id);
        if let Some(mut context) = held_context {
            context.managed_spec.admission = None;
            self.inner
                .admission_barrier
                .release(&record.root_session_id, &credential);
            self.inner.launch(&context)?;
            return Ok(true);
        }
        let pause = record.run_pause.as_ref().ok_or("not_held")?;
        if pause.parent_session_id != owner.parent_session_id
            || pause.parent_pause_seq != owner.parent_pause_seq
        {
            return Err("stale pause owner".into());
        }
        let parked_lease = lock(&self.inner.parked).get(task_id).cloned();
        let (cancel_resume, cancelled) = channel();
        let attempt = {
            let mut state = lock(&self.inner.state);
            state.next_resume_attempt += 1;
            let attempt = state.next_resume_attempt;
            if let Some((_, old)) = state
                .resume_attempts
                .insert(task_id.into(), (attempt, cancel_resume))
            {
                let _ = old.send(());
            }
            attempt
        };
        if let Some(lease) = parked_lease {
            let resumed = lock(&self.inner.concurrency).unpark(&lease);
            let grants = lock(&self.inner.concurrency).drain();
            for grant in grants {
                spawn_grant(grant);
            }
            resumed
                .recv_timeout(Duration::from_secs(30))
                .map_err(|e| e.to_string())?;
            if lock(&self.inner.concurrency).lease_state(task_id, record.notification.run_epoch)
                != Some("held")
            {
                return Err("resume suspended".into());
            }
        }
        if record.status == TaskStatus::Paused && lock(&self.inner.parked).get(task_id).is_none() {
            let (granted, received) = channel();
            let wake = lock(&self.inner.state)
                .resume_attempts
                .get(task_id)
                .map(|(_, wake)| wake.clone())
                .ok_or("resume suspended")?;
            lock(&self.inner.concurrency).enqueue_epoch(
                &record.model,
                task_id,
                record.notification.run_epoch,
                Box::new(move || {
                    let _ = granted.send(());
                    let _ = wake.send(());
                }),
            );
            let grants = lock(&self.inner.concurrency).drain();
            for grant in grants {
                spawn_grant(grant);
            }
            cancelled
                .recv_timeout(Duration::from_secs(30))
                .map_err(|e| e.to_string())?;
            if received.try_recv().is_err() {
                return Err("resume suspended".into());
            }
        }
        let current_attempt = lock(&self.inner.state)
            .resume_attempts
            .get(task_id)
            .map(|(id, _)| *id);
        if current_attempt != Some(attempt)
            || self.inner.try_load(task_id).is_none_or(|fresh| {
                fresh.run_pause != record.run_pause
                    || fresh.notification.run_epoch != record.notification.run_epoch
            })
        {
            return Err("stale resume attempt".into());
        }
        let result = native
            .resume_turn(
                handle.as_ref(),
                &crate::pause::ports::ResumeTurnRequest {
                    owner: owner.clone(),
                    resume_id: resume_id.into(),
                    child_seq_cutoff,
                },
            )
            .map_err(|e| e.to_string())?;
        match result {
            crate::pause::ports::ResumeChildTurnResult::OtherHoldLive { .. } => {
                lock(&self.inner.state).resume_receipts.insert(
                    (
                        task_id.into(),
                        record.notification.run_epoch,
                        resume_id.into(),
                    ),
                    false,
                );
                if let Some(lease) =
                    lock(&self.inner.concurrency).park(task_id, record.notification.run_epoch)
                {
                    lock(&self.inner.parked).insert(task_id.into(), lease);
                }
                lock(&self.inner.state).resume_attempts.remove(task_id);
                Ok(false)
            }
            crate::pause::ports::ResumeChildTurnResult::Resumed(
                crate::pause::ports::ResumedChildTurn { outcome, .. },
            ) => {
                let mut fresh = self
                    .inner
                    .try_load(task_id)
                    .ok_or("task record unavailable")?;
                if fresh.run_pause != record.run_pause
                    || fresh.notification.run_epoch != record.notification.run_epoch
                {
                    return Err("stale resume".into());
                }
                fresh.status = TaskStatus::Running;
                fresh.run_pause = None;
                self.inner
                    .store
                    .replace(&fresh)
                    .map_err(|e| e.to_string())?;
                lock(&self.inner.parked).remove(task_id);
                self.inner
                    .pauses_pending
                    .forget(task_id, record.notification.run_epoch);
                if let Err(error) = handle.publish_task_status(TaskStatus::Running) {
                    log(
                        "senpi-task running status publication failed",
                        task_id,
                        &error,
                    );
                }
                crate::manager::outcome::track_exact_outcome(
                    self.inner.arc(),
                    task_id.into(),
                    handle,
                    fresh.model,
                    fresh.notification.run_epoch,
                    outcome,
                );
                lock(&self.inner.state).resume_receipts.insert(
                    (
                        task_id.into(),
                        record.notification.run_epoch,
                        resume_id.into(),
                    ),
                    true,
                );
                self.inner
                    .admission_barrier
                    .release(&record.root_session_id, &credential);
                Ok(true)
            }
        }
    }

    pub fn hold_task_input(
        &self,
        task_id: &str,
        parent_session_id: &str,
        input_id: &str,
        revision: u64,
        text: &str,
    ) -> Result<bool, String> {
        let record = self
            .inner
            .try_load(task_id)
            .ok_or("task record unavailable")?;
        if record
            .run_pause
            .as_ref()
            .is_none_or(|p| p.parent_session_id != parent_session_id)
        {
            return Err("wrong_parent".into());
        }
        let handle = self
            .inner
            .live_handle(task_id)
            .ok_or("task is not resident")?;
        self.inner
            .child_pause
            .hold_input(
                handle.as_ref(),
                &crate::pause::ports::HoldInput {
                    input_id: input_id.into(),
                    revision,
                    text: text.into(),
                },
                parent_session_id,
            )
            .map(|result| {
                matches!(
                    result,
                    crate::pause::ports::EngineHoldInputResult::Held { .. }
                )
            })
            .map_err(|e| e.to_string())
    }

    pub fn continue_task(
        &self,
        task_id_or_name: &str,
        prompt: &str,
        deliver_as: Option<crate::state::DeliverAs>,
    ) -> Result<ContinueResult, SteeringError> {
        let outcome = self.send_to_task(&SendInput {
            id_or_name: task_id_or_name.to_string(),
            message: prompt.to_string(),
            deliver_as: Some(deliver_as.unwrap_or(crate::state::DeliverAs::FollowUp)),
            ..SendInput::default()
        })?;
        Ok(to_continue_result(outcome))
    }

    pub fn send_to_task(&self, input: &SendInput) -> Result<SendOutcome, SteeringError> {
        if let Some(record) = self
            .inner
            .resolve_cancel_id(&input.id_or_name)
            .and_then(|id| self.inner.try_load(&id))
        {
            if input
                .expected_run_epoch
                .is_some_and(|epoch| epoch != record.notification.run_epoch)
            {
                return Ok(SendOutcome::Stale {
                    task_id: record.task_id,
                    run_epoch: record.notification.run_epoch,
                    reason: "task run epoch changed".into(),
                });
            }
            if record.status != TaskStatus::Paused
                && !matches!(
                    self.inner.admission_barrier.state(&record.root_session_id),
                    crate::pause::admission_barrier::AdmissionState::Open
                )
            {
                let pause_seq = match self.inner.admission_barrier.state(&record.root_session_id) {
                    crate::pause::admission_barrier::AdmissionState::Closed { pause_seq }
                    | crate::pause::admission_barrier::AdmissionState::Resuming {
                        pause_seq, ..
                    } => pause_seq,
                    crate::pause::admission_barrier::AdmissionState::Open => 0,
                };
                let owner = crate::pause::ports::RunPauseOwner {
                    parent_session_id: record.root_session_id.clone(),
                    parent_pause_seq: pause_seq,
                    run_ref: crate::pause::ports::RunRef::task(record.task_id.clone()),
                };
                self.pause_task(&record.task_id, &owner)
                    .map_err(|e| SteeringError::Store(StoreError::Parse(e)))?;
                return self.send_to_task(input);
            }
            if record.status == TaskStatus::Paused
                && let Some(pause) = record.run_pause.as_ref() {
                    if input
                        .caller_session_id
                        .as_ref()
                        .is_some_and(|id| id != &pause.parent_session_id)
                        && !input.all_scope
                    {
                        return Ok(SendOutcome::ScopeDenied {
                            task_id: record.task_id,
                            owning_session_id: pause.parent_session_id.clone(),
                            reason: "held run belongs to another parent".into(),
                        });
                    }
                    let id = uuid::Uuid::new_v4().to_string();
                    self.hold_task_input(
                        &record.task_id,
                        &pause.parent_session_id,
                        &id,
                        0,
                        &input.message,
                    )
                    .map_err(|e| SteeringError::Store(StoreError::Parse(e)))?;
                    return Ok(SendOutcome::Held {
                        task_id: record.task_id,
                        pause_seq: pause.parent_pause_seq,
                        entry_id: id,
                    });
                }
        }
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
        // Only isolated live runs need settlement after cancellation releases handle ownership.
        let armed = self
            .inner
            .resolve_cancel_id(id_or_name)
            .and_then(|task_id| {
                let record = self.inner.try_load(&task_id)?;
                (record.status == TaskStatus::Running
                    && record.isolation.is_some()
                    && self.inner.live_handle(&task_id).is_some())
                .then_some((task_id, record.notification.run_epoch))
            });
        let mut guard = armed.map(|(task_id, epoch)| StopSettlementGuard {
            pending: self.inner.stops_pending.request(&task_id, epoch),
            stops: &self.inner.stops_pending,
            task_id,
            epoch,
            consumed: false,
            deferred: false,
        });
        let outcome = self
            .inner
            .steering
            .cancel_task(id_or_name, reason, options)?;
        if let CancelOutcome::Cancelled { task_id, .. } = &outcome {
            self.inner.release_slot_for_task(task_id);
            let grants = lock(&self.inner.concurrency).release_task(task_id);
            for grant in grants {
                spawn_grant(grant);
            }
            self.inner.settle_waiters(task_id);
            if let Some(guard) = guard.as_mut() {
                guard.consumed = true;
            }
        }
        if matches!(outcome, CancelOutcome::CancelPending { .. })
            && let Some(guard) = guard.as_mut()
        {
            guard.deferred = true;
        }
        Ok(outcome)
    }

    pub fn get(&self, task_id: &str) -> Option<TaskRecord> {
        self.inner.try_load(task_id)
    }

    // PERF-DIVERGENCE(senpi manager/manager.ts): upstream resolves a task name by listing every record (with queue positions); we scan with the store's early-exit find_record and skip queue positions. Same first match in list order.
    pub fn find_in_session_by_name(&self, session_id: &str, name: &str) -> Option<TaskRecord> {
        self.inner
            .store
            .find_record(|record| {
                in_session(record, session_id) && record.name.as_deref() == Some(name)
            })
            .ok()
            .flatten()
    }

    pub fn list(&self, scope: &ListScope) -> Vec<ListedTask> {
        let records = match self.inner.store.list_matching(|record| match scope {
            ListScope::All => true,
            ListScope::ParentSession(session_id) => in_session(record, session_id),
        }) {
            Ok(listed) => listed.records,
            Err(_) => return Vec::new(),
        };
        let concurrency = lock(&self.inner.concurrency);
        records
            .into_iter()
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
        // Register first, then read the record outside the state lock: a settle that lands after
        // the read removes this waiter (it takes the lock after its terminal write), so no wake is
        // lost, and the record file is no longer read under the manager-wide mutex.
        let (waiter_id, sender, receiver) = {
            let mut state = lock(&self.inner.state);
            let (sender, receiver) = channel();
            state.next_subscriber_id += 1;
            let waiter_id = state.next_subscriber_id;
            state
                .waiters
                .entry(id.clone())
                .or_default()
                .push((waiter_id, WaitWaker::Blocking(sender.clone())));
            (waiter_id, sender, receiver)
        };
        if let Some(current) = self.inner.try_load(&id)
            && is_terminal_record(&current)
        {
            self.inner.remove_waiter(&id, waiter_id);
            return Ok(current);
        }
        let _abort_waker = signal
            .map(|signal| AbortWakerGuard(signal, signal.register(WaitWaker::Blocking(sender))));
        let deadline = timeout.map(|timeout| std::time::Instant::now() + timeout);
        let woke = match deadline {
            None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(deadline) => {
                receiver.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            }
        };
        let error = match woke {
            Ok(WaitWake::Settled(record)) => return Ok(*record),
            Err(RecvTimeoutError::Disconnected) => return Err(WaitError::Aborted),
            Ok(WaitWake::Aborted) => WaitError::Aborted,
            Err(RecvTimeoutError::Timeout) => WaitError::TimedOut,
        };
        self.inner.remove_waiter(&id, waiter_id);
        while let Ok(wake) = receiver.try_recv() {
            if let WaitWake::Settled(record) = wake {
                return Ok(*record);
            }
        }
        Err(error)
    }

    /// Async twin of [`Self::wait_for`] with no timeout: registers before loading (no lost wake)
    /// and resolves when the task settles or `cancel` fires, without holding a thread.
    pub async fn wait_for_async(
        &self,
        task_id: &str,
        cancel: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<TaskRecord, WaitError> {
        let id = parse_task_id(task_id)?.to_string();
        let (mut receiver, _guard) = {
            let mut state = lock(&self.inner.state);
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            state.next_subscriber_id += 1;
            let waiter_id = state.next_subscriber_id;
            state
                .waiters
                .entry(id.clone())
                .or_default()
                .push((waiter_id, WaitWaker::Async(sender)));
            let guard = AsyncWaiterGuard {
                inner: Arc::clone(&self.inner),
                task_id: id.clone(),
                waiter_id,
            };
            (receiver, guard)
        };
        if let Some(current) = self.inner.try_load(&id)
            && is_terminal_record(&current)
        {
            return Ok(current);
        }
        tokio::select! {
            woke = receiver.recv() => match woke {
                Some(WaitWake::Settled(record)) => Ok(*record),
                Some(WaitWake::Aborted) | None => Err(WaitError::Aborted),
            },
            _changed = cancel.changed() => Err(WaitError::Aborted),
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

    pub fn active_parent_model_fallback(
        &self,
        task_id: &str,
    ) -> Option<(crate::state::ParentModelFallback, &'static str)> {
        let record = self.inner.try_load(task_id)?;
        let note = record.parent_model_fallback.clone()?;
        let parent = record.parent_model.as_deref()?;
        if let Some(observed) = &record.effective_model {
            if format!("{}/{}", observed.provider, observed.model_id) == parent {
                return Some((note, "observed"));
            }
            if observed.switched_by_operator == Some(true)
                || matches!(
                    observed.source,
                    crate::state::EffectiveModelSource::Set
                        | crate::state::EffectiveModelSource::Cycle
                        | crate::state::EffectiveModelSource::Restore
                        | crate::state::EffectiveModelSource::FallbackRevert
                )
            {
                return None;
            }
        }
        let selected = record.resolved_model.as_ref()?;
        if format!("{}/{}", selected.provider, selected.model_id) != parent {
            return None;
        }
        Some((
            note,
            if matches!(record.status, TaskStatus::Running | TaskStatus::Pending) {
                "switching"
            } else {
                "unconfirmed"
            },
        ))
    }

    pub fn live_run_stats(&self, task_id: &str) -> Option<(i64, TaskRunStats)> {
        let tracker = lock(&self.inner.state)
            .run_stats
            .get(task_id)
            .map(Arc::clone)?;
        let tracker = lock(&tracker);
        let epoch = tracker.run_start_epoch;
        Some((
            epoch,
            tracker.snapshot(u64::try_from(self.inner.now_ms()).unwrap_or_default()),
        ))
    }
    pub fn run_stats_snapshot(&self, task_id: &str) -> Option<TaskRunStats> {
        self.inner.run_stats_snapshot(task_id)
    }

    pub fn forget(&self, task_id: &str) {
        self.inner.residency.notify(
            self.inner
                .try_load(task_id)
                .as_ref()
                .map(|record| record.parent_session_id.as_str()),
        );
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

    pub fn is_launching(&self, task_id: &str) -> bool {
        lock(&self.inner.state).launching.contains(task_id)
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

    pub fn park_run_capacity(&self, task_id: &str, epoch: i64) {
        if let Some(lease) = lock(&self.inner.concurrency).park(task_id, epoch) {
            lock(&self.inner.parked).insert(task_id.into(), lease);
        }
    }

    pub fn acquire_resume_capacity(
        &self,
        task_id: &str,
        epoch: i64,
        allocation: Option<&crate::manager::concurrency::QueuedTaskAdmission>,
    ) -> Result<(), String> {
        let record = self
            .inner
            .try_load(task_id)
            .ok_or("task record unavailable")?;
        if record.notification.run_epoch != epoch {
            return Err("stale resume epoch".into());
        }
        let allocation = allocation.or(record.queued_admission.as_ref());
        if allocation.is_some_and(|saved| saved.run_epoch != epoch || saved.task_id != task_id) {
            return Err("stale resume allocation".into());
        }
        let lease = lock(&self.inner.parked).get(task_id).cloned();
        if let Some(lease) = lease {
            if lease.run_epoch != epoch {
                return Err("stale parked resume epoch".into());
            }
            let resumed = lock(&self.inner.concurrency).unpark(&lease);
            let grants = lock(&self.inner.concurrency).drain();
            for grant in grants {
                spawn_grant(grant);
            }
            resumed.recv().map_err(|error| error.to_string())?;
            if lock(&self.inner.concurrency).lease_state(task_id, epoch) != Some("held") {
                return Err("resume suspended".into());
            }
            let mut parked = lock(&self.inner.parked);
            if parked
                .get(task_id)
                .is_some_and(|current| Arc::ptr_eq(current, &lease))
            {
                parked.remove(task_id);
            }
            return Ok(());
        }
        if lock(&self.inner.concurrency).lease_state(task_id, epoch) == Some("held") {
            return Ok(());
        }
        let capacity = lock(&self.inner.resume_capacity);
        let (wake, received) = channel();
        let attempt = {
            let mut state = lock(&self.inner.state);
            state.next_resume_attempt += 1;
            let attempt = state.next_resume_attempt;
            if let Some((_, previous)) = state
                .resume_attempts
                .insert(task_id.into(), (attempt, wake.clone()))
            {
                let _ = previous.send(());
            }
            attempt
        };
        let model = allocation.map_or(record.model.as_str(), |saved| saved.model.as_str());
        let mut concurrency = lock(&self.inner.concurrency);
        concurrency.withdraw(model, task_id, epoch);
        let grant: Grant = Box::new(move || {
            let _ = wake.send(());
        });
        match allocation {
            Some(saved) => {
                let mut saved = saved.clone();
                saved.task_id = task_id.into();
                saved.run_epoch = epoch;
                concurrency.requeue(saved, grant);
            }
            None => {
                concurrency.enqueue_epoch(model, task_id, epoch, grant);
            }
        }
        let grants = concurrency.drain();
        drop(concurrency);
        drop(capacity);
        for grant in grants {
            spawn_grant(grant);
        }
        let waited = received.recv();
        let _capacity = lock(&self.inner.resume_capacity);
        let current = lock(&self.inner.state)
            .resume_attempts
            .get(task_id)
            .map(|(current, _)| *current);
        let held = lock(&self.inner.concurrency).lease_state(task_id, epoch) == Some("held");
        if current != Some(attempt) {
            return Err("resume suspended".into());
        }
        if waited.is_err() || !held {
            lock(&self.inner.concurrency).withdraw(model, task_id, epoch);
            lock(&self.inner.state).resume_attempts.remove(task_id);
            return Err("resume suspended".into());
        }
        lock(&self.inner.state).resume_attempts.remove(task_id);
        self.inner
            .store
            .mutate(task_id, |fresh: &TaskRecord| {
                if fresh.notification.run_epoch != epoch {
                    return fresh.clone();
                }
                TaskRecord {
                    queued_admission: None,
                    ..fresh.clone()
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn begin_native_pause(&self, task_id: &str, epoch: i64) {
        let _capacity = lock(&self.inner.resume_capacity);
        self.inner.pauses_pending.request(task_id, epoch);
        if let Some(record) = self.inner.try_load(task_id)
            && let Some(admission) =
                lock(&self.inner.concurrency).withdraw(&record.model, task_id, epoch)
                && let Err(error) = self.inner.store.mutate(task_id, |fresh: &TaskRecord| {
                    if fresh.notification.run_epoch != epoch {
                        return fresh.clone();
                    }
                    TaskRecord {
                        queued_admission: Some(admission.clone()),
                        ..fresh.clone()
                    }
                }) {
                    utils::logger::log(
                        "senpi-task failed to persist resume allocation",
                        Some(&json!({"taskId":task_id,"error":error.to_string()})),
                    );
                }
        if let Some(lease) = lock(&self.inner.parked).get(task_id).cloned() {
            lock(&self.inner.concurrency).suspend_unpark(&lease);
        }
        if let Some((_, wake)) = lock(&self.inner.state).resume_attempts.remove(task_id) {
            let _ = wake.send(());
        }
    }

    pub fn finish_native_pause(
        &self,
        task_id: &str,
        epoch: i64,
        accepted: bool,
    ) -> Result<(), String> {
        let result = if accepted {
            let mut changed = false;
            let result = self
                .inner
                .store
                .mutate(task_id, &mut |record: &TaskRecord| {
                    if record.notification.run_epoch != epoch
                        || !matches!(record.status, TaskStatus::Running | TaskStatus::Paused)
                    {
                        return record.clone();
                    }
                    changed = true;
                    TaskRecord {
                        status: TaskStatus::Paused,
                        ..record.clone()
                    }
                })
                .map_err(|error| error.to_string());
            if result.is_ok() && changed {
                self.inner.next_tracking_generation(task_id);
                if let Some(lease) = lock(&self.inner.concurrency).park(task_id, epoch) {
                    lock(&self.inner.parked).insert(task_id.into(), lease);
                }
                let grants = lock(&self.inner.concurrency).drain();
                for grant in grants {
                    spawn_grant(grant);
                }
            }
            result.map(|_| ())
        } else {
            Ok(())
        };
        self.inner.pauses_pending.settle(task_id, epoch);
        if !accepted {
            self.inner.pauses_pending.forget(task_id, epoch);
        }
        result
    }

    pub fn track_resumed_turn(
        &self,
        task_id: &str,
        handle: Arc<dyn ManagedChildHandle>,
        model: &str,
        epoch: i64,
        outcome: crate::manager::child_handle::ExactTurnOutcome,
    ) {
        crate::manager::outcome::track_exact_outcome(
            self.inner.arc(),
            task_id.into(),
            handle,
            model.into(),
            epoch,
            outcome,
        );
    }

    pub fn restore_paused_task(
        &self,
        task_id: &str,
    ) -> Result<Arc<dyn ManagedChildHandle>, String> {
        let record = self
            .inner
            .try_load(task_id)
            .ok_or("task record unavailable")?;
        let recorded = crate::lifecycle::host_session_resume_path(&record).map(PathBuf::from);
        let directory =
            crate::lifecycle::reconcile::child_session_dir(self.inner.store.state_dir(), task_id);
        let mut newest = None;
        if recorded.is_none() {
            for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
                let entry = entry.map_err(|error| error.to_string())?;
                let path = entry.path();
                let metadata = entry.metadata().map_err(|error| error.to_string())?;
                if metadata.is_file()
                    && path
                        .extension()
                        .is_some_and(|extension| extension == "jsonl")
                {
                    let candidate = (
                        metadata.modified().map_err(|error| error.to_string())?,
                        path,
                    );
                    if newest.as_ref().is_none_or(|best| &candidate > best) {
                        newest = Some(candidate);
                    }
                }
            }
        }
        let path = recorded
            .or_else(|| newest.map(|(_, path)| path))
            .ok_or("paused transcript unavailable; prompt replay is forbidden")?;
        self.restore_paused_child(task_id, &path)?;
        self.inner
            .live_handle(task_id)
            .ok_or("restored child is not attached".into())
    }

    pub fn restore_paused_child(&self, task_id: &str, session_path: &Path) -> Result<(), String> {
        let record = self
            .inner
            .try_load(task_id)
            .ok_or("task record unavailable")?;
        if record.status != TaskStatus::Paused {
            return Err("task is not paused".into());
        }
        if record.host_pid.is_some_and(|pid| {
            pid != self.inner.host_pid
                && u32::try_from(pid).is_ok_and(utils::process_sweep::default_is_process_alive)
        }) {
            return Err("paused owner is still alive".into());
        }
        if !session_path.is_file() {
            return Err("paused transcript unavailable; prompt replay is forbidden".into());
        }
        let mut claimed = false;
        let owned = self
            .inner
            .store
            .mutate(task_id, &mut |fresh: &TaskRecord| {
                if fresh.status != TaskStatus::Paused
                    || fresh.killed == Some(true)
                    || fresh.notification.run_epoch != record.notification.run_epoch
                    || fresh.host_pid != record.host_pid
                {
                    return fresh.clone();
                }
                claimed = true;
                TaskRecord {
                    host_pid: Some(self.inner.host_pid),
                    residency_state: ResidencyState::Resident,
                    ..fresh.clone()
                }
            })
            .map_err(|e| e.to_string())?
            .ok_or("task record unavailable")?;
        if !claimed {
            return Err("paused ownership changed".into());
        }
        let handle = match respawn_managed_task(&RespawnInput {
            record: &owned,
            session_path: Some(session_path),
            state_dir: self.inner.store.state_dir(),
            runners: &self.inner.runners,
            rpc_runner: self.inner.rpc_respawn_runner.as_ref(),
            trusted_launch: self.inner.trusted_respawn_launch.as_ref(),
            before_launch: &|| {
                if self.inner.try_load(task_id).is_some_and(|fresh| {
                    fresh.status == TaskStatus::Paused
                        && fresh.notification.run_epoch == owned.notification.run_epoch
                        && fresh.host_pid == Some(self.inner.host_pid)
                }) {
                    Ok(())
                } else {
                    Err(RespawnResult::Failed {
                        disposition: crate::lifecycle::port::RespawnDisposition::Retryable,
                        code: crate::lifecycle::port::RespawnFailureCode::RespawnFailed,
                        reason: "paused ownership changed".into(),
                    })
                }
            },
        }) {
            RespawnResult::Ok(handle) => handle,
            RespawnResult::Failed { reason, .. } => return Err(reason),
        };
        let saved = owned.run_pause.as_ref().ok_or("paused owner unavailable")?;
        let owner = crate::pause::ports::RunPauseOwner {
            parent_session_id: saved.parent_session_id.clone(),
            parent_pause_seq: saved.parent_pause_seq,
            run_ref: crate::pause::ports::RunRef {
                id: saved.run_ref.id.clone(),
                kind: match &saved.run_ref.kind {
                    crate::state::RunPauseKind::Task => crate::pause::ports::RunRefKind::Task,
                    crate::state::RunPauseKind::Dag => crate::pause::ports::RunRefKind::Dag,
                    crate::state::RunPauseKind::Team => crate::pause::ports::RunRefKind::Team,
                    crate::state::RunPauseKind::Workpool => {
                        crate::pause::ports::RunRefKind::Workpool
                    }
                },
            },
        };
        let held = self
            .inner
            .child_pause
            .pause_for_run(handle.as_ref(), &owner);
        match held {
            Ok(crate::pause::ports::EngineInheritReceipt::Inherited { .. }) => {}
            result => {
                if let Err(error) = discard_managed_handle(handle.as_ref()) {
                    log("senpi-task paused restore cleanup failed", task_id, &error);
                }
                return Err(match result {
                    Err(error) => error.to_string(),
                    _ => "wrong_parent".into(),
                });
            }
        }
        match self.reattach(&owned, handle) {
            ReattachResult::Ok => Ok(()),
            ReattachResult::Failed { reason, .. } => Err(reason),
        }
    }

    pub fn settle_launches_for_shutdown(&self, bound: Duration) {
        let deadline = std::time::Instant::now() + bound;
        let inner = Arc::clone(&self.inner);
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let settle = || {
                for runner in [&inner.runners.in_process, &inner.runners.process] {
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        runner.settle_teardowns()
                    })) {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => log("senpi-task runner teardown failed", "", &error),
                        Err(_) => log("senpi-task runner teardown panicked", "", &"runner panic"),
                    }
                }
            };
            settle();
            let state = lock(&inner.state);
            let waited = inner
                .launches_changed
                .wait_timeout_while(
                    state,
                    deadline.saturating_duration_since(std::time::Instant::now()),
                    |state| !state.launching.is_empty(),
                )
                .unwrap_or_else(PoisonError::into_inner);
            drop(waited);
            settle();
            let _ = tx.send(());
        });
        if let Err(error) =
            rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
        {
            log("senpi-task shutdown settlement incomplete", "", &error);
        }
        let held: Vec<_> = {
            let state = lock(&self.inner.state);
            state
                .held_launches
                .values()
                .chain(state.queued_launches.values())
                .cloned()
                .collect()
        };
        for context in held {
            let task_id = &context.record.task_id;
            lock(&self.inner.concurrency).withdraw(
                &context.model,
                task_id,
                context.record.notification.run_epoch,
            );
            if let Some(lease) =
                lock(&self.inner.concurrency).park(task_id, context.record.notification.run_epoch)
            {
                lock(&self.inner.parked).insert(task_id.clone(), lease);
            }
            if let Some(admission) = &context.managed_spec.admission {
                admission.leave();
            }
        }
    }

    pub fn respawn(
        &self,
        record: &TaskRecord,
        resume_session_path: Option<&Path>,
    ) -> RespawnResult {
        let ticket = match self.inner.admission_barrier.enter(
            &record.root_session_id,
            crate::pause::admission_barrier::AdmissionParticipant {
                key: record.task_id.clone(),
                run_ref: crate::pause::ports::RunRef::task(&record.task_id),
            },
            None,
        ) {
            crate::pause::admission_barrier::AdmissionEntry::Entered { ticket } => ticket,
            crate::pause::admission_barrier::AdmissionEntry::Held { .. } => {
                return RespawnResult::Failed {
                    disposition: crate::lifecycle::port::RespawnDisposition::Retryable,
                    code: crate::lifecycle::port::RespawnFailureCode::RunPaused,
                    reason: "run_paused".into(),
                };
            }
        };
        struct TicketGuard(crate::pause::admission_barrier::AdmissionTicket);
        impl Drop for TicketGuard {
            fn drop(&mut self) {
                self.0.leave();
            }
        }
        let _ticket_guard = TicketGuard(ticket.clone());
        if self
            .inner
            .try_load(&record.task_id)
            .is_some_and(|fresh| fresh.run_pause.is_some())
            || !matches!(
                self.inner.admission_barrier.state(&record.root_session_id),
                crate::pause::admission_barrier::AdmissionState::Open
            )
        {
            return RespawnResult::Failed {
                disposition: crate::lifecycle::port::RespawnDisposition::Retryable,
                code: crate::lifecycle::port::RespawnFailureCode::RunPaused,
                reason: "run_paused".into(),
            };
        }
        // Respawn bypasses start's status transition; persist the same durable launch boundary
        // without changing the status or epoch that lifecycle reattachment owns. It is also the last
        // point before the child runs: a stop, a kill or another owner that landed while the revival
        // was preparing refuses the launch here (TS `beforeLaunch`).
        let before_launch = || -> Result<(), RespawnResult> {
            if ticket.check() != crate::pause::admission_barrier::AdmissionCheck::Allowed {
                return Err(RespawnResult::Failed {
                    disposition: crate::lifecycle::port::RespawnDisposition::Retryable,
                    code: crate::lifecycle::port::RespawnFailureCode::RunPaused,
                    reason: "run_paused".into(),
                });
            }
            if self
                .inner
                .try_load(&record.task_id)
                .is_some_and(|fresh| fresh.run_pause.is_some())
                || !matches!(
                    self.inner.admission_barrier.state(&record.root_session_id),
                    crate::pause::admission_barrier::AdmissionState::Open
                )
            {
                return Err(RespawnResult::Failed {
                    disposition: crate::lifecycle::port::RespawnDisposition::Retryable,
                    code: crate::lifecycle::port::RespawnFailureCode::RunPaused,
                    reason: "run_paused".into(),
                });
            }
            let mut moved = false;
            let timestamp = self.inner.now_iso();
            let stamped = self
                .inner
                .store
                .mutate(&record.task_id, &mut |fresh: &TaskRecord| {
                    moved = (is_terminal_record(fresh) && !is_terminal_record(record))
                        || fresh.killed == Some(true)
                        || fresh.host_pid != record.host_pid
                        || fresh.notification.run_epoch != record.notification.run_epoch;
                    if moved || fresh.started_at.is_some() {
                        fresh.clone()
                    } else {
                        let mut next = fresh.clone();
                        next.started_at = Some(timestamp.clone());
                        next
                    }
                });
            match stamped {
                Ok(Some(_)) if !moved => Ok(()),
                Ok(Some(_)) => Err(RespawnResult::Failed {
                    disposition: crate::lifecycle::port::RespawnDisposition::Retryable,
                    code: crate::lifecycle::port::RespawnFailureCode::RespawnFailed,
                    reason: "task was stopped or moved before its respawn launched".to_string(),
                }),
                Ok(None) => Err(RespawnResult::Failed {
                    disposition: crate::lifecycle::port::RespawnDisposition::Unrecoverable,
                    code: crate::lifecycle::port::RespawnFailureCode::SpawnSpecUnavailable,
                    reason: format!("Task record not found before respawn: {}", record.task_id),
                }),
                Err(error) => Err(RespawnResult::Failed {
                    disposition: crate::lifecycle::port::RespawnDisposition::Retryable,
                    code: crate::lifecycle::port::RespawnFailureCode::RespawnFailed,
                    reason: error.to_string(),
                }),
            }
        };
        respawn_managed_task(&RespawnInput {
            record,
            session_path: resume_session_path,
            state_dir: self.inner.store.state_dir(),
            runners: &self.inner.runners,
            rpc_runner: self.inner.rpc_respawn_runner.as_ref(),
            trusted_launch: self.inner.trusted_respawn_launch.as_ref(),
            before_launch: &before_launch,
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

fn member_kernel_tool_refusal(spec: &ManagerStartSpec) -> Option<StartResult> {
    if spec.team_role.as_deref() != Some("member") || spec.kernel_tools.is_none() {
        return None;
    }
    Some(StartResult::PlanUnresolved(PlanResolutionError::new(
        PlanResolutionCode::InvalidTarget,
        "Team members run out of process and cannot reach a parent JavaScript kernel, so they never receive parent kernel tools.",
    )))
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
    let stop_requested = this.clone();
    let stop_settled = this.clone();
    let stats = this;
    SteeringPort {
        send_while_run_held: None,
        store,
        live_handle: Arc::new(move |task_id| live.upgrade()?.live_handle(task_id)),
        dequeue_pending: Arc::new(move |task_id| {
            dequeue
                .upgrade()
                .is_some_and(|inner| inner.dequeue_pending(task_id))
        }),
        reacquire_for_revive: Arc::new(move |task_id| {
            if let Some(inner) = revive.upgrade()
                && let Some(record) = inner.try_load(task_id)
            {
                inner.reacquire_for_revive(task_id, record.notification.run_epoch);
            }
        }),
        destruction,
        run_stats_snapshot: Arc::new(move |task_id| stats.upgrade()?.run_stats_snapshot(task_id)),
        stop_requested: Some(Arc::new(move |task_id| {
            if let Some(inner) = stop_requested.upgrade()
                && let Some(record) = inner.try_load(task_id)
            {
                inner
                    .stops_pending
                    .request(task_id, record.notification.run_epoch);
            }
        })),
        stop_settled: Some(Arc::new(move |task_id| {
            if let Some(inner) = stop_settled.upgrade()
                && let Some(record) = inner.try_load(task_id)
            {
                inner
                    .stops_pending
                    .settle(task_id, record.notification.run_epoch);
            }
        })),
        now,
    }
}

impl Inner {
    fn resolve_cancel_id(&self, id_or_name: &str) -> Option<String> {
        if let Some(record) = self.try_load(id_or_name) {
            return Some(record.task_id);
        }
        // First record in `list` order with this name; stops reading at the match instead of
        // loading and cloning every record in the state dir.
        self.store
            .find_record(|record| record.name.as_deref() == Some(id_or_name))
            .ok()?
            .map(|record| record.task_id)
    }

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
        let tracker = lock(&self.state).run_stats.get(task_id).map(Arc::clone)?;
        let snapshot = lock(&tracker).snapshot(now);
        Some(snapshot)
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
        admission_lease: Option<Arc<dyn SessionAdmissionLease>>,
    ) -> StartResult {
        let mut parent_plan = plan.clone();
        let parent = spec
            .parent_model
            .as_deref()
            .filter(|_| spec.category.is_some() && spec.model.is_none())
            .and_then(|value| {
                let (provider, model) = value.split_once('/')?;
                if provider.is_empty() || model.is_empty() {
                    return None;
                }
                Some(crate::state::ResolvedModelRecord::new(
                    crate::state::ResolvedModelSource::Category,
                    provider,
                    model,
                ))
            })
            .filter(|parent| {
                plan.model != parent.display
                    && !plan
                        .resolved_model
                        .iter()
                        .chain(plan.requested_model.iter())
                        .chain(plan.fallback_models.iter().flatten())
                        .any(|m| m.provider == parent.provider && m.model_id == parent.model_id)
            });
        if let Some(parent) = &parent {
            parent_plan
                .fallback_models
                .get_or_insert_with(Vec::new)
                .push(parent.clone());
        }
        let plan = &parent_plan;
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
            if let Some(lease) = &admission_lease {
                lease.release();
            }
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
            Ok(mut record) => {
                if let Some(parent) = &parent {
                    record.parent_model = Some(parent.display.clone());
                    if let Err(error) = self.store.replace(&record) {
                        log(
                            "senpi-task parent rung persistence failed",
                            &record.task_id,
                            &error,
                        );
                    }
                }
                record
            }
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
                if let Some(lease) = &admission_lease {
                    lease.release();
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
        if let Some(admission) = &spec.admission {
            admission.bind(&claimed.task_id);
        }
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
        // Isolation is built BEFORE the record is committed to a launch: a repository that cannot be
        // cloned must refuse the spawn, never silently run the child against the parent checkout,
        // because "isolated" is a safety promise the caller relies on.
        let prepared: Option<PreparedIsolation> = if self.isolation.isolates(spec) {
            match self.isolation.prepare(spec, &claimed.task_id) {
                IsolationPreparation::Prepared {
                    handle,
                    baseline,
                    spec: isolation_spec,
                } => Some((*handle, *baseline, isolation_spec)),
                IsolationPreparation::Refused { reason } => {
                    if Some(registration.name.as_str()) != claimed.name.as_deref() {
                        lock(&self.names).release(&spec.parent_session_id, &registration.name);
                    }
                    lock(&self.state).background.remove(&claimed.task_id);
                    let message = format!("isolation_unavailable: {reason}");
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
                            error_message: message.clone(),
                            killed: false,
                            run_stats: None,
                        },
                    );
                    assert!(
                        started && failed,
                        "isolation refusal transitions were not applied"
                    );
                    if let Some(lease) = &admission_lease {
                        lease.release();
                    }
                    return StartResult::StartFailed(StartFailure {
                        task_id: claimed.task_id.clone(),
                        name: registration.name.clone(),
                        category: claimed.category.clone(),
                        subagent_type: claimed.agent_type.clone(),
                        execution_mode,
                        model: claimed.model.clone(),
                        resolved_model: claimed.resolved_model.clone(),
                        run_in_background: spec.run_in_background,
                        error_message: message,
                    });
                }
            }
        } else {
            None
        };
        let isolation_cwd = prepared
            .as_ref()
            .map_or_else(|| self.cwd.clone(), |(_, _, spec)| spec.merged_dir.clone());
        let managed_spec = build_managed_spec(
            &renamed,
            spec,
            plan,
            &isolation_cwd,
            self.store.state_dir(),
            self.traceparent_for
                .as_ref()
                .and_then(|resolve| resolve(&spec.parent_session_id)),
        );
        let spawn_spec = match &prepared {
            Some((_, _, isolation_spec)) => SpawnSpecV1 {
                isolation: Some(isolation_spec.clone()),
                ..build_spawn_spec_v1(&managed_spec)
            },
            None => build_spawn_spec_v1(&managed_spec),
        };
        let final_record = TaskRecord {
            isolation: prepared
                .as_ref()
                .map(|(_, _, isolation_spec)| IsolationRecord::new(isolation_spec.clone())),
            spawn_spec: Some(TaskSpawnSpec::V1(spawn_spec)),
            ..renamed
        };
        if let Err(error) = self.store.replace(&final_record) {
            log(
                "senpi-task manager spawn bookkeeping failed",
                &claimed.task_id,
                &error,
            );
            if prepared.is_some() {
                self.isolation.discard(&claimed.task_id);
            }
            if let Some(lease) = &admission_lease {
                lease.release();
            }
            return self.bookkeeping_failed(spec, &claimed, &registration.name, execution_mode);
        }
        if let Some(lease) = &admission_lease {
            lease.release();
        }
        if let Some((handle, baseline, _)) = prepared {
            self.isolation.bind(&final_record.task_id, handle, baseline);
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
        if let Some(admission) = &context.managed_spec.admission {
            admission.transfer();
        }
        let immediate = {
            let mut concurrency = lock(&self.concurrency);
            if concurrency.has_free_slot(&plan.model) {
                let acquired = concurrency.try_acquire(
                    &plan.model,
                    &final_record.task_id,
                    final_record.notification.run_epoch,
                );
                debug_assert!(acquired);
                None
            } else {
                lock(&self.state)
                    .queued_launches
                    .insert(final_record.task_id.clone(), context.clone());
                let inner = self.arc();
                let queued = context.clone();
                let grant: Grant = Box::new(move || {
                    let _outcome_recorded_on_record = inner.launch(&queued);
                });
                Some(concurrency.enqueue_epoch(
                    &plan.model,
                    &final_record.task_id,
                    final_record.notification.run_epoch,
                    grant,
                ))
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
        if let Some(ticket) = &context.managed_spec.admission {
            ticket.creating();
        }
        lock(&self.state).queued_launches.remove(&record.task_id);
        let epoch = record.notification.run_epoch;
        if !context.managed_spec.admission.as_ref().map_or_else(
            || {
                matches!(
                    self.admission_barrier.state(&record.root_session_id),
                    crate::pause::admission_barrier::AdmissionState::Open
                )
            },
            |scope| scope.check() == crate::pause::admission_barrier::AdmissionCheck::Allowed,
        ) {
            lock(&self.state)
                .held_launches
                .insert(record.task_id.clone(), context.clone());
            if let Some(lease) = lock(&self.concurrency).park(&record.task_id, epoch) {
                lock(&self.parked).insert(record.task_id.clone(), lease);
            }
            return Ok(());
        }
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
        let started = self.start_marked(context);
        let handle = match started {
            Ok(handle) => handle,
            Err(error) => {
                let message = public_start_failure_message(&error);
                self.release_slot(&record.task_id, &context.model, epoch);
                self.isolation.discard(&record.task_id);
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
        let current = self.try_load(&record.task_id);
        let cancelled = current
            .as_ref()
            .is_some_and(|current| current.status == TaskStatus::Cancelled);
        let held_creation = current.as_ref().is_some_and(|fresh| {
            fresh.status == TaskStatus::Paused
                && fresh.notification.run_epoch == epoch
                && fresh.host_pid == record.host_pid
        });
        if !held_creation && !owns_run(&launch_run_of(&context.record), current.as_ref()) {
            self.discard_stale_launch(context, handle);
            self.end_launch(&record.task_id);
            return Err(if cancelled {
                "task was cancelled during launch"
            } else {
                "task stopped during launch"
            }
            .to_string());
        }
        self.attach_launched(context, &handle);
        if let Some(admission) = &context.managed_spec.admission {
            admission.leave();
        }
        self.end_launch(&record.task_id);
        if let Err(error) = self.steering.notify_started(&record.task_id) {
            log(
                "senpi-task manager notify started failed",
                &record.task_id,
                &error,
            );
        }
        Ok(())
    }

    /// Starts the child while the task is marked launching, so a reconcile on this host cannot read
    /// a started-but-unregistered child as an orphan and revive it over the still-running process.
    /// The mark is cleared only after the caller attaches or discards the handle (see
    /// [`Self::end_launch`]) — clearing it here would reopen the window before `live` holds it.
    fn start_marked(&self, context: &LaunchContext) -> crate::manager::types::ManagedRunnerResult {
        lock(&self.state)
            .launching
            .insert(context.record.task_id.clone());
        let mut attempt = context.clone();
        let started = loop {
            let context = &attempt;
            let started = if let Some(created) = context.runner.start_staged(&context.managed_spec)
            {
                created.and_then(|staged| {
                    let root = &context.record.root_session_id;
                    if context.managed_spec.admission.as_ref().map_or_else(
                        || {
                            matches!(
                                self.admission_barrier.state(root),
                                crate::pause::admission_barrier::AdmissionState::Open
                            )
                        },
                        |ticket| {
                            matches!(
                                ticket.check(),
                                crate::pause::admission_barrier::AdmissionCheck::Allowed
                            )
                        },
                    ) {
                        let turn = (staged.begin)();
                        lock(&self.state)
                            .initial_turns
                            .insert(context.record.task_id.clone(), turn.outcome);
                    } else {
                        let pause_seq = match self.admission_barrier.state(root) {
                            crate::pause::admission_barrier::AdmissionState::Closed {
                                pause_seq,
                            }
                            | crate::pause::admission_barrier::AdmissionState::Resuming {
                                pause_seq,
                                ..
                            } => pause_seq,
                            crate::pause::admission_barrier::AdmissionState::Open => 0,
                        };
                        let owner = crate::pause::ports::RunPauseOwner {
                            parent_session_id: root.clone(),
                            parent_pause_seq: pause_seq,
                            run_ref: crate::pause::ports::RunRef::task(
                                context.record.task_id.clone(),
                            ),
                        };
                        let held = self
                            .child_pause
                            .pause_for_run(staged.child.as_ref(), &owner)
                            .and_then(|receipt| match receipt {
                                crate::pause::ports::EngineInheritReceipt::Inherited { .. } => self
                                    .child_pause
                                    .hold_input(
                                        staged.child.as_ref(),
                                        &crate::pause::ports::HoldInput {
                                            input_id: "initial".into(),
                                            revision: 0,
                                            text: staged.first_prompt.clone(),
                                        },
                                        root,
                                    )
                                    .map(|_| ()),
                                crate::pause::ports::EngineInheritReceipt::WrongParent => {
                                    Err(crate::host::HostError {
                                        message: "wrong_parent".into(),
                                    })
                                }
                            });
                        if let Err(error) = held {
                            if let Err(cleanup) = discard_managed_handle(staged.child.as_ref()) {
                                log(
                                    "senpi-task held creation cleanup failed",
                                    &context.record.task_id,
                                    &cleanup,
                                );
                            }
                            return Err(ManagedRunnerError::Other(error.to_string()));
                        }
                        if let Some(mut record) = self.try_load(&context.record.task_id) {
                            record.status = TaskStatus::Paused;
                            record.run_pause = Some(crate::state::RecordedRunPause {
                                parent_session_id: root.clone(),
                                parent_pause_seq: pause_seq,
                                run_ref: crate::state::RunPauseRef {
                                    kind: crate::state::RunPauseKind::Task,
                                    id: record.task_id.clone(),
                                },
                            });
                            self.store
                                .replace(&record)
                                .map_err(|e| ManagedRunnerError::Other(e.to_string()))?;
                        }
                        if let Some(lease) = lock(&self.concurrency).park(
                            &context.record.task_id,
                            context.record.notification.run_epoch,
                        ) {
                            lock(&self.parked).insert(context.record.task_id.clone(), lease);
                        }
                    }
                    Ok(staged.child)
                })
            } else {
                context.runner.start(&context.managed_spec)
            };
            match started {
                Ok(handle) => {
                    lock(&self.state)
                        .started_contexts
                        .insert(context.record.task_id.clone(), context.clone());
                    break Ok(handle);
                }
                Err(error) => {
                    let Some(next) = self.advance_start_fallback(context, &error) else {
                        break Err(error);
                    };
                    attempt = next;
                }
            }
        };
        if started.is_err() {
            self.end_launch(&context.record.task_id);
        }
        started
    }

    fn advance_start_fallback(
        &self,
        context: &LaunchContext,
        error: &ManagedRunnerError,
    ) -> Option<LaunchContext> {
        let ManagedRunnerError::Runner(failure) = error else {
            return None;
        };
        if !matches!(
            failure.kind,
            RunnerFailureKind::ChildPromptFailed
                | RunnerFailureKind::ChildTurnFailed
                | RunnerFailureKind::ModelUnavailable
        ) {
            return None;
        }
        let record = self.try_load(&context.record.task_id)?;
        if record.status != TaskStatus::Running
            || record.notification.run_epoch != context.record.notification.run_epoch
            || record.host_pid != Some(self.host_pid)
        {
            return None;
        }
        let candidates = crate::manager::credential_failure::runtime_fallback_candidates(
            &record,
            &failure.message,
        );
        let next = candidates.remaining.first()?.clone();
        let remaining = candidates.chain_after_hop();
        let mut attempts = record
            .fallback_attempts
            .clone()
            .unwrap_or_else(|| record.resolved_model.iter().cloned().collect());
        attempts.push(next.clone());
        let note = (record.parent_model.as_deref() == Some(next.display.as_str())).then(|| {
            let reason = crate::manager::parent_model_fallback::reason_of_failure(
                &failure.message,
                record
                    .resolved_model
                    .as_ref()
                    .map(|model| model.model_id.as_str()),
            );
            let reason = if reason == crate::state::ParentModelFallbackReason::Error {
                crate::state::ParentModelFallbackReason::Unavailable
            } else {
                reason
            };
            crate::manager::parent_model_fallback::build(
                record.model.clone(),
                reason,
                Some(&failure.message),
                &[],
                self.now_iso(),
            )
        });
        let updated = TaskRecord {
            model: next.display.clone(),
            resolved_model: Some(next.clone()),
            fallback_models: Some(remaining.clone()),
            fallback_attempts: Some(attempts),
            parent_model_fallback: note.or(record.parent_model_fallback.clone()),
            ..record.clone()
        };
        let mut committed = false;
        let persisted = self
            .store
            .mutate(&record.task_id, &mut |fresh: &TaskRecord| {
                if fresh.status != TaskStatus::Running
                    || fresh.notification.run_epoch != record.notification.run_epoch
                    || fresh.host_pid != record.host_pid
                    || fresh.model != record.model
                {
                    return fresh.clone();
                }
                committed = true;
                TaskRecord {
                    model: updated.model.clone(),
                    resolved_model: updated.resolved_model.clone(),
                    fallback_models: updated.fallback_models.clone(),
                    fallback_attempts: updated.fallback_attempts.clone(),
                    parent_model_fallback: updated.parent_model_fallback.clone(),
                    ..fresh.clone()
                }
            });
        if let Err(error) = persisted {
            log(
                "senpi-task start fallback persistence failed",
                &record.task_id,
                &error,
            );
            return None;
        }
        if !committed {
            return None;
        }
        Some(LaunchContext {
            model: next.display.clone(),
            record: updated,
            runner: Arc::clone(&context.runner),
            managed_spec: ManagedStartSpec {
                model: Some(next.display.clone()),
                resolved_model: Some(next),
                fallback_models: Some(remaining),
                ..context.managed_spec.clone()
            },
        })
    }

    fn end_launch(&self, task_id: &str) {
        lock(&self.state).launching.remove(task_id);
        self.launches_changed.notify_all();
    }

    /// `#discardStaleLaunch`: a child whose start resolved after its launch went stale is torn down,
    /// never attached. This run's own cancel goes through the destruction port so the disposal is
    /// recorded; any other stale child is discarded directly.
    fn discard_stale_launch(&self, context: &LaunchContext, handle: Arc<dyn ManagedChildHandle>) {
        let task_id = &context.record.task_id;
        let epoch = context.record.notification.run_epoch;
        let current = self.try_load(task_id);
        if current.as_ref().is_some_and(|current| {
            current.status == TaskStatus::Cancelled && current.notification.run_epoch == epoch
        }) {
            lock(&self.state).live.insert(
                task_id.clone(),
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
                .destroy_resident_task(task_id, DestroyCause::Cancel)
            {
                log(
                    "senpi-task manager cancel-during-launch destroy failed",
                    task_id,
                    &error,
                );
            }
        } else if let Err(error) = discard_managed_handle(handle.as_ref()) {
            log(
                "senpi-task manager stale launch discard failed",
                task_id,
                &error,
            );
        }
        self.release_slot(task_id, &context.model, epoch);
        self.settle_waiters(task_id);
    }

    fn attach_launched(&self, context: &LaunchContext, handle: &Arc<dyn ManagedChildHandle>) {
        let started = lock(&self.state)
            .started_contexts
            .remove(&context.record.task_id);
        let context = started.as_ref().unwrap_or(context);
        let task_id = &context.record.task_id;
        lock(&self.state).live.insert(
            task_id.clone(),
            LiveTask {
                handle: Arc::clone(handle),
                model: context.model.clone(),
                unsubscribe: None,
                managed_spec: Some(context.managed_spec.clone()),
                runner: Some(Arc::clone(&context.runner)),
            },
        );
        let unsubscribe = self.subscribe_child_facts(handle, task_id);
        if let Some(live) = lock(&self.state).live.get_mut(task_id) {
            live.unsubscribe = Some(unsubscribe);
        }
        self.attach_child_subscribers(task_id, handle);
        self.record_spawn_facts(task_id, handle.as_ref());
        // Stamp the child's own identity onto the clone so a sweep can tell a live child from a dead host.
        self.isolation.stamp(task_id, handle.as_ref());
        if self
            .try_load(task_id)
            .is_some_and(|r| r.status == TaskStatus::Paused)
        {
            return;
        }
        let exact = lock(&self.state).initial_turns.remove(task_id);
        match exact {
            Some(outcome) => crate::manager::outcome::track_exact_outcome(
                self.arc(),
                task_id.clone(),
                Arc::clone(handle),
                context.model.clone(),
                context.record.notification.run_epoch,
                outcome,
            ),
            None => track_outcome(
                self.arc(),
                task_id.clone(),
                Arc::clone(handle),
                context.model.clone(),
                context.record.notification.run_epoch,
            ),
        }
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
        let mut tracker_value = create_run_stats_tracker(started_at, clock);
        tracker_value.run_start_epoch = self
            .try_load(task_id)
            .map_or(0, |record| record.run_start_epoch);
        let tracker = Arc::new(Mutex::new(tracker_value));
        lock(&self.state)
            .run_stats
            .insert(task_id.to_string(), Arc::clone(&tracker));
        let stats = handle.subscribe(Arc::new(move |event| {
            // `RunStatsTracker::accept` ignores every other event type (e.g. streaming deltas).
            if !matches!(
                event.event_type.as_str(),
                "tool_execution_start" | "tool_execution_end" | "message_start" | "message_end"
            ) {
                return;
            }
            lock(&tracker).accept(event);
        }));
        let epoch = self.try_load(task_id).map(|record| record.run_start_epoch);
        let owner = Arc::downgrade(handle);
        let manager = self.this.clone();
        let id = task_id.to_string();
        let stamped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let identity_owner = Arc::downgrade(handle);
        let identity_manager = self.this.clone();
        let identity_task = task_id.to_string();
        let identity_stamp = stamped.clone();
        let refresh_identity: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let (Some(inner), Some(handle)) =
                (identity_manager.upgrade(), identity_owner.upgrade())
            else {
                return;
            };
            let Some(record) = inner.try_load(&identity_task) else {
                return;
            };
            if Some(record.run_start_epoch) != epoch
                || identity_stamp.load(std::sync::atomic::Ordering::Acquire)
                || !inner
                    .live_handle(&identity_task)
                    .is_some_and(|live| Arc::ptr_eq(&live, &handle))
            {
                return;
            }
            let (Some(transport), Some(identity)) = (&inner.host_transport, &record.host_session)
            else {
                return;
            };
            match transport.durable_session_id(identity) {
                Ok(Some(session_id)) if !session_id.is_empty() => {
                    if identity_stamp.swap(true, std::sync::atomic::Ordering::AcqRel) {
                        return;
                    }
                    if let Err(error) =
                        inner
                            .store
                            .mutate(&identity_task, |fresh: &TaskRecord| {
                                if Some(fresh.run_start_epoch) != epoch
                                    || fresh.host_pid != record.host_pid
                                {
                                    return fresh.clone();
                                }
                                TaskRecord {
                                    child_session_id: Some(session_id.clone()),
                                    ..fresh.clone()
                                }
                            })
                    {
                        log(
                            "senpi-task host durable identity persistence failed",
                            &identity_task,
                            &error,
                        );
                    }
                }
                Ok(_) => {}
                Err(error) => utils::logger::log(
                    "senpi-task host identity lookup failed",
                    Some(&json!({"taskId":identity_task,"error":error})),
                ),
            }
        });
        let initial_identity = refresh_identity.clone();
        std::thread::spawn(move || initial_identity());
        let host_identity = handle.subscribe(Arc::new(move |event| {
            if matches!(
                event.event_type.as_str(),
                "session_info_changed" | "session_replaced" | "agent_start" | "message_end"
            ) {
                let refresh = refresh_identity.clone();
                std::thread::spawn(move || refresh());
            }
        }));
        let durable = handle.on_durable_session_id(Arc::new(move |session_id| {
            let (Some(inner), Some(handle)) = (manager.upgrade(), owner.upgrade()) else {
                return;
            };
            if !inner
                .live_handle(&id)
                .is_some_and(|live| Arc::ptr_eq(&live, &handle))
            {
                return;
            }
            let Some(record) = inner.try_load(&id) else {
                return;
            };
            if Some(record.run_start_epoch) != epoch || session_id.is_empty() {
                return;
            }
            if stamped.swap(true, std::sync::atomic::Ordering::AcqRel) {
                return;
            }
            if let Err(error) = inner.store.mutate(&id, |fresh: &TaskRecord| {
                if Some(fresh.run_start_epoch) != epoch || fresh.host_pid != record.host_pid {
                    return fresh.clone();
                }
                TaskRecord {
                    child_session_id: Some(session_id.to_string()),
                    ..fresh.clone()
                }
            }) {
                log(
                    "senpi-task late durable identity persistence failed",
                    &id,
                    &error,
                );
            }
        }));
        let fallback_store = self.store.clone();
        let fallback_id = task_id.to_string();
        let failed: Arc<Mutex<(Option<String>, Vec<String>)>> =
            Arc::new(Mutex::new((None, Vec::new())));
        lock(&self.state)
            .failed_turns
            .insert(task_id.into(), Arc::clone(&failed));
        let streamed = Arc::new(Mutex::new(Vec::<String>::new()));
        let fallback = handle.subscribe(Arc::new(move |event| {
            if event.event_type == "message_start" {
                lock(&streamed).clear();
            }
            if matches!(event.event_type.as_str(), "message_update" | "message_end")
                && let Some(message) = event.message.as_ref().filter(|m| {
                    m.get("role").and_then(serde_json::Value::as_str) == Some("assistant")
                }) {
                    let tools = message
                        .get("content")
                        .and_then(serde_json::Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|part| part["type"] == "toolCall")
                        .filter_map(|part| part["name"].as_str())
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    if event.event_type == "message_update" {
                        *lock(&streamed) = tools;
                    } else {
                        let mut facts = lock(&failed);
                        if matches!(message["stopReason"].as_str(), Some("error" | "aborted")) {
                            let mut names = Vec::new();
                            for name in tools.iter().chain(lock(&streamed).iter()) {
                                if !names.contains(name) {
                                    names.push(name.clone());
                                }
                            }
                            let message_text = message["errorMessage"]
                                .as_str()
                                .filter(|text| !text.is_empty())
                                .map(str::to_owned)
                                .unwrap_or_else(|| {
                                    format!(
                                        "stopped with {}",
                                        message["stopReason"].as_str().unwrap_or_default()
                                    )
                                });
                            *facts = (Some(message_text), names);
                        } else {
                            *facts = (None, Vec::new());
                        }
                        return;
                    }
                }
            if event.event_type != "retry_fallback_applied" {
                return;
            }
            let Ok(Some(mut record)) = fallback_store.load(&fallback_id) else {
                return;
            };
            if !event.to.as_deref().is_some_and(|to| {
                record.parent_model.as_deref().is_some_and(|parent| {
                    to == parent
                        || crate::manager::parent_model_fallback::base_selector(to) == parent
                })
            }) {
                return;
            }
            let mut facts = lock(&failed);
            let detail = facts.0.as_deref();
            let classified = crate::manager::parent_model_fallback::reason_of_failure(
                detail.unwrap_or_default(),
                record
                    .resolved_model
                    .as_ref()
                    .map(|model| model.model_id.as_str()),
            );
            let reason = if event.limit.is_some() {
                crate::state::ParentModelFallbackReason::UsageLimit
            } else if classified != crate::state::ParentModelFallbackReason::Error {
                classified
            } else {
                match event.reason.as_deref() {
                    Some("billing") => crate::state::ParentModelFallbackReason::UsageLimit,
                    Some("refusal") => crate::state::ParentModelFallbackReason::Refused,
                    Some("transient") => crate::state::ParentModelFallbackReason::Unavailable,
                    _ => crate::state::ParentModelFallbackReason::Error,
                }
            };
            record.parent_model_fallback = Some(crate::manager::parent_model_fallback::build(
                crate::manager::parent_model_fallback::base_selector(
                    event.from.as_deref().unwrap_or(&record.model),
                )
                .into(),
                reason,
                detail,
                &facts.1,
                chrono::Utc::now().to_rfc3339(),
            ));
            *facts = (None, Vec::new());
            let note = record.parent_model_fallback.clone();
            if let Err(error) =
                fallback_store.mutate(&fallback_id, |fresh: &TaskRecord| TaskRecord {
                    parent_model_fallback: note.clone(),
                    ..fresh.clone()
                })
            {
                log(
                    "senpi-task parent fallback fact persistence failed",
                    &fallback_id,
                    &error,
                );
            }
        }));
        let resumed_manager = self.this.clone();
        let resumed_id = task_id.to_string();
        let resumed_handle = Arc::downgrade(handle);
        let self_resumed = handle.on_self_resumed(Arc::new(move || {
            let (Some(inner), Some(handle)) = (resumed_manager.upgrade(), resumed_handle.upgrade())
            else {
                return;
            };
            if !inner
                .live_handle(&resumed_id)
                .is_some_and(|live| Arc::ptr_eq(&live, &handle))
            {
                return;
            }
            let Some(mut record) = inner.try_load(&resumed_id) else {
                return;
            };
            if !record.status.is_terminal() {
                return;
            }
            record.notification.run_epoch += 1;
            record.run_start_epoch = record.notification.run_epoch;
            record.status = TaskStatus::Running;
            record.final_response = None;
            record.error_message = None;
            if let Some(tracker) = lock(&inner.state).run_stats.get(&resumed_id).cloned() {
                lock(&tracker).run_start_epoch = record.run_start_epoch;
            }
            if let Err(error) = inner.store.replace(&record) {
                log("senpi-task self-resumed reopen failed", &resumed_id, &error);
                return;
            }
            let publication = lock(&inner.host_status_publication);
            if let (Some(transport), Some(identity)) =
                (&inner.host_transport, record.host_session.as_ref())
                && let Err(error) = transport.publish_task_status(identity, TaskStatus::Running) {
                    utils::logger::log(
                        "senpi-task daemon running publication failed",
                        Some(&json!({"taskId":resumed_id,"error":error})),
                    );
                }
            if let Err(error) = handle.publish_task_status(TaskStatus::Running) {
                log(
                    "senpi-task self-resumed running publication failed",
                    &resumed_id,
                    &error,
                );
            }
            drop(publication);
            inner.reacquire_for_revive(&resumed_id, record.notification.run_epoch);
        }));
        Box::new(move || {
            host_identity();
            transcript();
            stats();
            fallback();
            if let Some(self_resumed) = self_resumed {
                self_resumed();
            }
            if let Some(durable) = durable {
                durable();
            }
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
        let candidates = crate::manager::credential_failure::runtime_fallback_candidates(
            &record,
            &input.failure.message,
        );
        let next_model = candidates.remaining.first()?.clone();
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
        let remaining = crate::manager::credential_failure::runtime_fallback_candidates(
            &record,
            &input.failure.message,
        )
        .chain_after_hop();
        let mut attempts = record
            .fallback_attempts
            .clone()
            .unwrap_or_else(|| record.resolved_model.iter().cloned().collect());
        attempts.push(next_model.clone());
        let mut notification = record.notification.clone();
        notification.run_epoch += 1;
        let failed = lock(&self.state).failed_turns.get(&input.task_id).cloned();
        let facts = failed.as_ref().map(|facts| lock(facts));
        let message = facts
            .as_ref()
            .and_then(|facts| facts.0.as_deref())
            .unwrap_or(&input.failure.message);
        let tools = facts.as_ref().map_or(&[][..], |facts| facts.1.as_slice());
        let parent_note = (record.parent_model.as_deref() == Some(next_model.display.as_str()))
            .then(|| {
                let reason = crate::manager::parent_model_fallback::reason_of_failure(
                    message,
                    record
                        .resolved_model
                        .as_ref()
                        .map(|model| model.model_id.as_str()),
                );
                crate::manager::parent_model_fallback::build(
                    record.model.clone(),
                    reason,
                    Some(message),
                    tools,
                    input.timestamp.clone(),
                )
            });
        let next_record = TaskRecord {
            parent_model_fallback: parent_note.or(record.parent_model_fallback.clone()),
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
        let context_epoch = context.record.notification.run_epoch;
        let inner = self.arc();
        let grant: Grant = Box::new(move || inner.launch_runtime_fallback(&context));
        let immediate = {
            let mut concurrency = lock(&self.concurrency);
            if concurrency.has_free_slot(&next_model.display) {
                let acquired =
                    concurrency.try_acquire(&next_model.display, &input.task_id, context_epoch);
                debug_assert!(acquired);
                Some(grant)
            } else {
                concurrency.enqueue_epoch(
                    &next_model.display,
                    &input.task_id,
                    context_epoch,
                    grant,
                );
                None
            }
        };
        if let Some(grant) = immediate {
            spawn_grant(grant);
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
        match self.start_marked(context) {
            // The start blocked: a cancel, interrupt or another owner may have moved the task meanwhile.
            // A late child of a run that is no longer this one's is discarded, never attached.
            Ok(handle)
                if !owns_run(
                    &launch_run_of(&context.record),
                    self.try_load(task_id).as_ref(),
                ) =>
            {
                self.discard_stale_launch(context, handle);
                self.end_launch(task_id);
            }
            Ok(handle) => {
                self.attach_launched(context, &handle);
                self.end_launch(task_id);
            }
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

    fn reacquire_for_revive(&self, task_id: &str, epoch: i64) {
        let Some((handle, model)) = lock(&self.state)
            .live
            .get(task_id)
            .map(|live| (Arc::clone(&live.handle), live.model.clone()))
        else {
            return;
        };
        lock(&self.concurrency).try_acquire(&model, task_id, epoch);
        lock(&self.state)
            .released
            .insert(task_id.to_string(), epoch.saturating_sub(1));
        let now = self.now.clone();
        let clock: Box<dyn Fn() -> u64 + Send> =
            Box::new(move || u64::try_from(now()).unwrap_or_default());
        let started_at = u64::try_from(self.now_ms()).unwrap_or_default();
        // PERF-DIVERGENCE(senpi manager/manager.ts): the live event closure holds the tracker Arc, so a revive resets the shared tracker in place instead of replacing the map entry.
        let mut fresh_tracker = create_run_stats_tracker(started_at, clock);
        fresh_tracker.run_start_epoch = self
            .try_load(task_id)
            .map_or(0, |record| record.run_start_epoch);
        let existing = lock(&self.state).run_stats.get(task_id).map(Arc::clone);
        match existing {
            Some(tracker) => *lock(&tracker) = fresh_tracker,
            None => {
                lock(&self.state)
                    .run_stats
                    .insert(task_id.to_string(), Arc::new(Mutex::new(fresh_tracker)));
            }
        }
        if let Some(tracker) = lock(&self.state).run_stats.get(task_id).cloned() {
            lock(&tracker).run_start_epoch = epoch;
        }
        track_outcome(self.arc(), task_id.to_string(), handle, model, epoch);
    }

    fn settle_failed_workpool_revival(
        &self,
        task_id: &str,
        epoch: i64,
        reason: &str,
    ) -> Result<(), SteeringError> {
        let model = lock(&self.state)
            .live
            .get(task_id)
            .map(|live| live.model.clone());
        let transition = self.store.transition(
            task_id,
            &TaskTransition::Fail {
                timestamp: self.now_iso(),
                error_message: reason.to_string(),
                killed: false,
                run_stats: None,
            },
        );
        if let Some(model) = &model {
            self.release_slot(task_id, model, epoch);
        }
        self.settle_waiters(task_id);
        let transition = transition?;
        if !transition.applied {
            return Err(SteeringError::Store(crate::store::StoreError::Parse(
                "failed workpool revival did not reach terminal state".into(),
            )));
        }
        if let Err(error) = self
            .destruction
            .destroy_resident_task(task_id, DestroyCause::Cancel)
        {
            log(
                "senpi-task manager failed workpool revival destruction failed",
                task_id,
                &error,
            );
        }
        Ok(())
    }

    fn release_slot(&self, task_id: &str, _model: &str, epoch: i64) {
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
        let grants = {
            let mut concurrency = lock(&self.concurrency);
            if concurrency.lease_state(task_id, epoch).is_some() {
                concurrency.release_lease(task_id, epoch)
            } else {
                concurrency.release_task(task_id)
            }
        };
        for grant in grants {
            spawn_grant(grant);
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
        if is_terminal_record(&record) {
            if let Some(handle) = self.live_handle(task_id) {
                let status = record.status;
                let task_id = task_id.to_string();
                let manager = self.this.clone();
                std::thread::spawn(move || {
                    let Some(inner) = manager.upgrade() else {
                        return;
                    };
                    let _publication = lock(&inner.host_status_publication);
                    let status = inner
                        .try_load(&task_id)
                        .map_or(status, |record| record.status);
                    if let Some(identity) = inner
                        .try_load(&task_id)
                        .and_then(|record| record.host_session)
                        && let Some(transport) = &inner.host_transport
                            && let Err(error) = transport.publish_task_status(&identity, status) {
                                utils::logger::log(
                                    "senpi-task daemon status publication failed",
                                    Some(&json!({"taskId":task_id,"error":error})),
                                );
                            }
                    if let Err(error) = handle.publish_task_status(status) {
                        log(
                            "senpi-task host task status publication failed",
                            &task_id,
                            &error,
                        );
                    }
                });
            }
            {
                let mut state = lock(&self.state);
                if let Some(live) = state.live.get_mut(task_id) {
                    live.managed_spec = None;
                    live.runner = None;
                }
                state.run_stats.remove(task_id);
            }
            self.residency.notify(Some(&record.parent_session_id));
        }
        let waiters = lock(&self.state)
            .waiters
            .remove(task_id)
            .unwrap_or_default();
        for (_, waiter) in waiters {
            let _receiver_gone = waiter.send(WaitWake::Settled(Box::new(record.clone())));
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
        lock(&self.state).live.insert(
            fresh.task_id.clone(),
            LiveTask {
                handle: Arc::clone(&handle),
                model: fresh.model.clone(),
                unsubscribe: None,
                managed_spec: None,
                runner: None,
            },
        );
        self.attach_child_subscribers(&fresh.task_id, &handle);
        // Stamp the child's own identity onto the clone so a sweep can tell a live child from a dead host.
        self.isolation.stamp(&fresh.task_id, handle.as_ref());
        let pid = handle.pid().or(fresh.pid);
        if is_terminal_record(&fresh) || fresh.status == TaskStatus::Paused {
            if fresh.status == TaskStatus::Paused {
                self.admission_barrier.close(
                    &fresh.root_session_id,
                    fresh.run_pause.as_ref().map_or(1, |p| p.parent_pause_seq),
                );
                let acquired = lock(&self.concurrency).try_acquire(
                    &fresh.model,
                    &fresh.task_id,
                    fresh.notification.run_epoch,
                );
                if acquired
                    && let Some(lease) =
                        lock(&self.concurrency).park(&fresh.task_id, fresh.notification.run_epoch)
                    {
                        lock(&self.parked).insert(fresh.task_id.clone(), lease);
                    }
            }
            let updated = TaskRecord {
                updated_at: self.now_iso(),
                pid,
                ..fresh.clone()
            };
            let result = match self.store.replace(&updated) {
                Ok(()) => ReattachResult::Ok,
                Err(error) => return self.reattach_failed(&fresh.task_id, &handle, &error),
            };
            let unsubscribe = self.subscribe_child_facts(&handle, &fresh.task_id);
            if let Some(live) = lock(&self.state).live.get_mut(&fresh.task_id) {
                live.unsubscribe = Some(unsubscribe);
            }
            return result;
        }
        let mut notification = fresh.notification.clone();
        notification.run_epoch += 1;
        let epoch = notification.run_epoch;
        let reattached = TaskRecord {
            status: TaskStatus::Running,
            updated_at: self.now_iso(),
            started_at: fresh.started_at.clone().or_else(|| Some(self.now_iso())),
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
        let unsubscribe = self.subscribe_child_facts(&handle, &reattached.task_id);
        if let Some(live) = lock(&self.state).live.get_mut(&reattached.task_id) {
            live.unsubscribe = Some(unsubscribe);
        }
        lock(&self.concurrency).try_acquire(&reattached.model, &reattached.task_id, epoch);
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
    fn next_tracking_generation(&self, task_id: &str) -> u64 {
        let mut state = lock(&self.state);
        let generation = state
            .tracking_generations
            .entry(task_id.to_string())
            .or_default();
        *generation += 1;
        *generation
    }
    fn tracking_generation(&self, task_id: &str) -> u64 {
        lock(&self.state)
            .tracking_generations
            .get(task_id)
            .copied()
            .unwrap_or(0)
    }
    fn pause_settlement(
        &self,
        task_id: &str,
        epoch: i64,
    ) -> Option<crate::manager::outcome::PendingStop> {
        self.pauses_pending.take_settlement(task_id, epoch)
    }

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

    fn settle_isolation(&self, task_id: &str, merge: bool) {
        self.isolation.settle(task_id, merge);
    }

    fn stop_settlement(
        &self,
        task_id: &str,
        epoch: i64,
    ) -> Option<crate::manager::outcome::PendingStop> {
        self.stops_pending.take_settlement(task_id, epoch)
    }

    fn outcome_processed(&self) {
        lock(&self.state).processed_outcomes += 1;
    }
}
