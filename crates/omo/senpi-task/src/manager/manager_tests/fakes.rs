//! Rust port of `manager/__fixtures__/manager-fakes.ts`: a scripted runner whose handles settle on
//! demand, plus manager/lifecycle composition helpers. `wait_until` replaces `await flush()`: every
//! fake event (runner start, subscribe, abort, dispose) notifies one condvar; manager-internal state
//! (waiters, released slots) has no event hook, so each wait slice is also capped at `RECHECK`.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::host::HostError;
use crate::lifecycle::port::{ResidencyRegistry, ResidentHandle, ResidentKind};
use crate::lifecycle::{
    AdmissionResult, DestroyCause, LifecycleDeps, TaskLifecycle, TaskSettings,
    create_task_lifecycle,
};
use crate::manager::concurrency::TaskConcurrencyConfig;
use crate::manager::execution_mode::ExecutionMode;
use crate::manager::types::{
    ChildPlanner, ManagedRunner, ManagedRunnerError, ManagedRunnerResult, ManagedRunners,
    ManagedStartSpec, ManagerConfig, ManagerStartSpec, ResolvedChildPlan, SpawnAdmission,
    StartResult, StartedTask, TaskManagerOptions,
};
use crate::manager::{
    ManagedChildHandle, ManagedChildListener, TaskManager, Unsubscribe, create_task_manager,
};
use crate::runners::{RunnerFailure, RunnerFailureKind, RunnerOutcome};
use crate::shared::ManagedChildEvent;
use crate::state::ResidencyState;
use crate::steering::DestructionPort;
use crate::store::{StateDirConfig, TaskRecordStore};
use crate::team::member_projection::ResidentSessionRef;
use crate::team::runtime_types::{
    TeamCancelOutcome, TeamMemberCancelPort, TeamMemberReadPort, TeamMemberStartSpec,
    TeamMemberTaskRecord, TeamRuntimeManagerPort, TeamStartResult, TeamStartedMember,
};

pub(crate) const WAIT: Duration = Duration::from_secs(5);
const RECHECK: Duration = Duration::from_millis(5);

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

static EVENTS: (Mutex<u64>, Condvar) = (Mutex::new(0), Condvar::new());

pub(crate) fn notify() {
    *lock(&EVENTS.0) += 1;
    EVENTS.1.notify_all();
}

/// Waits for fake-event notifications until `condition` holds; panics with `what` after [`WAIT`].
pub(crate) fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        let seen = *lock(&EVENTS.0);
        if condition() {
            return;
        }
        let mut generation = lock(&EVENTS.0);
        if *generation == seen {
            let now = Instant::now();
            assert!(now < deadline, "timed out waiting for {what}");
            generation = EVENTS
                .1
                .wait_timeout(generation, (deadline - now).min(RECHECK))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

#[derive(Default)]
struct HandleState {
    listeners: Vec<(u64, ManagedChildListener)>,
    next_listener: u64,
    subscribe_calls: usize,
    unsubscribe_calls: usize,
    steer_calls: Vec<String>,
    follow_up_calls: Vec<String>,
    abort_calls: usize,
    dispose_calls: usize,
}

pub(crate) struct FakeHandle {
    task_id: String,
    pid: Option<i64>,
    state: Arc<Mutex<HandleState>>,
    outcomes: Mutex<Receiver<RunnerOutcome>>,
    settle: Mutex<Sender<RunnerOutcome>>,
}

impl FakeHandle {
    pub fn new(task_id: &str, pid: Option<i64>) -> Arc<Self> {
        let (sender, receiver) = channel();
        Arc::new(Self {
            task_id: task_id.to_string(),
            pid,
            state: Arc::default(),
            outcomes: Mutex::new(receiver),
            settle: Mutex::new(sender),
        })
    }

    pub fn settle(&self, outcome: RunnerOutcome) {
        lock(&self.settle)
            .send(outcome)
            .expect("outcome receiver alive");
    }

    pub fn complete(&self, final_response: &str) {
        self.settle(RunnerOutcome::Completed {
            final_response: final_response.to_string(),
        });
    }

    pub fn fail(&self, kind: RunnerFailureKind, message: &str) {
        self.settle(RunnerOutcome::Error {
            failure: RunnerFailure {
                kind,
                message: message.to_string(),
            },
            killed: false,
        });
    }

    pub fn emit(&self, event: &ManagedChildEvent) {
        let listeners: Vec<ManagedChildListener> = lock(&self.state)
            .listeners
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect();
        for listener in listeners {
            listener(event);
        }
    }

    pub fn subscribe_count(&self) -> usize {
        lock(&self.state).subscribe_calls
    }

    pub fn unsubscribe_count(&self) -> usize {
        lock(&self.state).unsubscribe_calls
    }

    pub fn steer_calls(&self) -> Vec<String> {
        lock(&self.state).steer_calls.clone()
    }

    pub fn follow_up_calls(&self) -> Vec<String> {
        lock(&self.state).follow_up_calls.clone()
    }

    pub fn abort_calls(&self) -> usize {
        lock(&self.state).abort_calls
    }

    pub fn dispose_calls(&self) -> usize {
        lock(&self.state).dispose_calls
    }
}

impl ManagedChildHandle for FakeHandle {
    fn task_id(&self) -> &str {
        &self.task_id
    }

    fn session_id(&self) -> Option<String> {
        Some(format!("sess-{}", self.task_id))
    }

    fn pid(&self) -> Option<i64> {
        self.pid
    }

    fn steer(&self, text: &str) -> Result<(), HostError> {
        lock(&self.state).steer_calls.push(text.to_string());
        Ok(())
    }

    fn follow_up(&self, text: &str) -> Result<(), HostError> {
        lock(&self.state).follow_up_calls.push(text.to_string());
        Ok(())
    }

    fn abort(&self) -> Result<(), HostError> {
        lock(&self.state).abort_calls += 1;
        notify();
        Ok(())
    }

    fn subscribe(&self, listener: ManagedChildListener) -> Unsubscribe {
        let id = {
            let mut state = lock(&self.state);
            state.subscribe_calls += 1;
            state.next_listener += 1;
            let id = state.next_listener;
            state.listeners.push((id, listener));
            id
        };
        notify();
        let state = Arc::clone(&self.state);
        Box::new(move || {
            let mut state = lock(&state);
            state.unsubscribe_calls += 1;
            state
                .listeners
                .retain(|(listener_id, _)| *listener_id != id);
        })
    }

    fn wait_for_outcome(&self) -> RunnerOutcome {
        lock(&self.outcomes)
            .recv()
            .unwrap_or(RunnerOutcome::Cancelled)
    }

    fn last_assistant_text(&self) -> Option<String> {
        None
    }

    fn dispose(&self) -> Result<(), HostError> {
        lock(&self.state).dispose_calls += 1;
        notify();
        Ok(())
    }
}

pub(crate) type StartHook =
    Arc<dyn Fn(&ManagedStartSpec, usize) -> Option<ManagedRunnerResult> + Send + Sync>;

#[derive(Default)]
pub(crate) struct FakeRunner {
    pub handles: Mutex<HashMap<String, Arc<FakeHandle>>>,
    pub started_specs: Mutex<Vec<ManagedStartSpec>>,
    pub start_error: Mutex<Option<ManagedRunnerError>>,
    pub child_pid: Mutex<Option<i64>>,
    pub hook: Mutex<Option<StartHook>>,
}

impl FakeRunner {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn throw_on_start(&self, enabled: bool) {
        *lock(&self.start_error) =
            enabled.then(|| ManagedRunnerError::Other("runner boom".to_string()));
    }

    pub fn handle(&self, task_id: &str) -> Option<Arc<FakeHandle>> {
        lock(&self.handles).get(task_id).cloned()
    }

    pub fn wait_handle(&self, task_id: &str) -> Arc<FakeHandle> {
        wait_until(&format!("runner handle for {task_id}"), || {
            self.handle(task_id).is_some()
        });
        self.handle(task_id).expect("handle present")
    }

    pub fn started_count(&self) -> usize {
        lock(&self.started_specs).len()
    }

    pub fn specs(&self) -> Vec<ManagedStartSpec> {
        lock(&self.started_specs).clone()
    }
}

impl ManagedRunner for FakeRunner {
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult {
        // `started_count() >= call` is how tests detect a launch happened; if it became visible
        // before the corresponding handle was resolvable via `handles`, a concurrent
        // `wait_handle(task_id)` could race ahead and return the *previous* launch's handle for
        // the same task_id (fallback handoffs reuse the task id across launches). The success
        // path inserts the handle into `handles` under the same `started_specs` lock acquisition
        // that records the call, so `started_count() >= call` can only become true once the
        // matching handle is already there; `notify()` fires last, after either outcome.
        let fake = FakeHandle::new(&spec.task_id, *lock(&self.child_pid));
        {
            let mut specs = lock(&self.started_specs);
            // Evict the previous launch's handle before the new count becomes visible, so a
            // `wait_handle(task_id)` that follows `started_count() >= call` can never resolve to it.
            lock(&self.handles).remove(&spec.task_id);
            specs.push(spec.clone());
            let call = specs.len();
            drop(specs);
            let hook = lock(&self.hook).clone();
            if let Some(hook) = hook
                && let Some(result) = hook(spec, call)
            {
                notify();
                return result;
            }
            if let Some(error) = lock(&self.start_error).clone() {
                notify();
                return Err(error);
            }
            lock(&self.handles).insert(spec.task_id.clone(), Arc::clone(&fake));
        }
        notify();
        Ok(fake)
    }
}

pub(crate) fn config(default_concurrency: usize, max_depth: u32) -> ManagerConfig {
    ManagerConfig {
        concurrency: TaskConcurrencyConfig {
            default_concurrency: Some(default_concurrency),
            provider_concurrency: None,
            model_concurrency: None,
        },
        max_depth,
        default_execution_mode: ExecutionMode::InProcess,
    }
}

pub(crate) fn category_planner(models: &[(&str, &str)]) -> ChildPlanner {
    let models: HashMap<String, String> = models
        .iter()
        .map(|(key, model)| ((*key).to_string(), (*model).to_string()))
        .collect();
    Arc::new(move |spec: &ManagerStartSpec| {
        let key = spec
            .category
            .clone()
            .or(spec.subagent_type.clone())
            .unwrap_or_else(|| "default".to_string());
        let model = spec
            .model
            .clone()
            .or_else(|| models.get(&key).cloned())
            .unwrap_or_else(|| "anthropic/claude".to_string());
        Ok(ResolvedChildPlan {
            model,
            category: spec.category.clone(),
            agent_type: spec.subagent_type.clone(),
            ..ResolvedChildPlan::default()
        })
    })
}

pub(crate) fn base_spec() -> ManagerStartSpec {
    ManagerStartSpec {
        prompt: "do the thing".to_string(),
        parent_session_id: "parent-1".to_string(),
        depth: 1,
        category: Some("quick".to_string()),
        ..ManagerStartSpec::default()
    }
}

pub(crate) fn named(name: &str) -> ManagerStartSpec {
    ManagerStartSpec {
        name: Some(name.to_string()),
        ..base_spec()
    }
}

/// A temp project dir; clones share it, so a second manager can reopen the same store.
#[derive(Clone)]
pub(crate) struct Project {
    pub dir: Arc<tempfile::TempDir>,
}

impl Project {
    pub fn new() -> Self {
        Self {
            dir: Arc::new(tempfile::tempdir().expect("tempdir")),
        }
    }

    pub fn store(&self) -> TaskRecordStore {
        TaskRecordStore::new(&StateDirConfig {
            project_dir: self.dir.path().to_path_buf(),
            task_state_dir: None,
        })
    }

    pub fn cwd(&self) -> String {
        self.dir.path().to_string_lossy().into_owned()
    }
}

pub(crate) struct Harness {
    pub manager: TaskManager,
    pub store: TaskRecordStore,
    pub in_process: Arc<FakeRunner>,
    pub process: Arc<FakeRunner>,
    pub project: Project,
}

pub(crate) type Customize = Box<dyn FnOnce(&mut TaskManagerOptions)>;

#[derive(Default)]
pub(crate) struct HarnessOptions {
    pub config: Option<ManagerConfig>,
    pub planner: Option<ChildPlanner>,
    pub in_process: Option<Arc<FakeRunner>>,
    pub process: Option<Arc<FakeRunner>>,
    pub customize: Option<Customize>,
    /// Reuse an existing project (and so its store) instead of a fresh temp dir.
    pub project: Option<Project>,
}

pub(crate) fn make_manager(options: HarnessOptions) -> Harness {
    let project = options.project.unwrap_or_else(Project::new);
    let store = project.store();
    let in_process = options.in_process.unwrap_or_default();
    let process = options.process.unwrap_or_default();
    let mut manager_options = TaskManagerOptions::new(
        store.clone(),
        ManagedRunners {
            in_process: Arc::clone(&in_process) as Arc<dyn ManagedRunner>,
            process: Arc::clone(&process) as Arc<dyn ManagedRunner>,
        },
        options.planner.unwrap_or_else(|| category_planner(&[])),
        project.cwd(),
    );
    manager_options.config = options.config.unwrap_or_else(|| config(5, 1));
    if let Some(customize) = options.customize {
        customize(&mut manager_options);
    }
    Harness {
        manager: create_task_manager(manager_options),
        store,
        in_process,
        process,
        project,
    }
}

pub(crate) fn default_manager() -> Harness {
    make_manager(HarnessOptions::default())
}

pub(crate) fn started(result: StartResult) -> StartedTask {
    match result {
        StartResult::Started(task) => task,
        other => panic!("expected started, got {other:?}"),
    }
}

pub(crate) fn status_of(
    store: &TaskRecordStore,
    task_id: &str,
) -> Option<crate::state::TaskStatus> {
    store
        .load(task_id)
        .ok()
        .flatten()
        .map(|record| record.status)
}

/// Blocks on the manager's own terminal waiter (event-driven, bounded by [`WAIT`]).
pub(crate) fn wait_terminal(manager: &TaskManager, task_id: &str) -> crate::state::TaskRecord {
    manager
        .wait_for(task_id, None, Some(WAIT))
        .unwrap_or_else(|error| panic!("{task_id} never settled: {error}"))
}

pub(crate) fn lifecycle_settings(overrides: Value) -> TaskSettings {
    TaskSettings::resolve(&overrides).expect("valid task settings")
}

struct ManagerResident(Arc<dyn ManagedChildHandle>);

impl ResidentHandle for ManagerResident {
    fn task_id(&self) -> &str {
        self.0.task_id()
    }

    fn kind(&self) -> ResidentKind {
        if self.0.pid().is_none() {
            ResidentKind::InProcess
        } else {
            ResidentKind::Rpc
        }
    }

    fn pid(&self) -> Option<i64> {
        self.0.pid()
    }

    fn abort(&self) -> Result<(), HostError> {
        self.0.abort()
    }

    fn dispose(&self) -> Result<(), HostError> {
        self.0.dispose()
    }

    fn terminate(&self) -> Result<(), HostError> {
        if self.0.pid().is_some() {
            self.0.abort()
        } else {
            Ok(())
        }
    }
}

#[derive(Default)]
struct ManagerRegistry(Mutex<Option<TaskManager>>);

impl ManagerRegistry {
    fn manager(&self) -> TaskManager {
        lock(&self.0)
            .clone()
            .expect("manager accessed before test composition finished")
    }
}

impl ResidencyRegistry for ManagerRegistry {
    fn get(&self, task_id: &str) -> Option<Arc<dyn ResidentHandle>> {
        self.manager()
            .get_resident_handle(task_id)
            .map(|handle| Arc::new(ManagerResident(handle)) as Arc<dyn ResidentHandle>)
    }

    fn entries(&self) -> Vec<Arc<dyn ResidentHandle>> {
        let manager = self.manager();
        manager
            .resident_task_ids()
            .iter()
            .filter_map(|task_id| manager.get_resident_handle(task_id))
            .map(|handle| Arc::new(ManagerResident(handle)) as Arc<dyn ResidentHandle>)
            .collect()
    }

    fn forget(&self, task_id: &str) {
        self.manager().forget(task_id);
    }

    fn has_pending_sends(&self, _task_id: &str) -> bool {
        false
    }
}

struct LifecycleDestruction(Arc<Mutex<Option<Arc<TaskLifecycle>>>>);

impl DestructionPort for LifecycleDestruction {
    fn destroy_resident_task(&self, task_id: &str, _cause: DestroyCause) -> Result<(), HostError> {
        let lifecycle = lock(&self.0).clone().expect("lifecycle composed");
        lifecycle
            .destroy_resident_task(task_id, DestroyCause::Cancel)
            .map_err(|error| HostError {
                message: error.to_string(),
            })
    }
}

/// The TS `spawnTeamMembers({ manager })` call hands the real manager straight to the runtime; the
/// Rust runtime takes the narrow [`TeamRuntimeManagerPort`], so the manager tests adapt a real
/// [`TaskManager`] here (the same shape `tools/team` uses for its collision test).
pub(crate) struct TeamPortManager {
    manager: TaskManager,
    specs: Mutex<HashMap<String, TeamMemberStartSpec>>,
}

impl TeamPortManager {
    pub(crate) fn new(manager: TaskManager) -> Self {
        Self {
            manager,
            specs: Mutex::new(HashMap::new()),
        }
    }
}

impl TeamMemberReadPort for TeamPortManager {
    fn get(&self, task_id: &str) -> Option<TeamMemberTaskRecord> {
        let record = self.manager.get(task_id)?;
        let spec = lock(&self.specs).get(task_id).cloned()?;
        let child_session_id = self
            .manager
            .get_resident_handle(task_id)
            .and_then(|handle| handle.session_id());
        Some(TeamMemberTaskRecord {
            task_id: task_id.to_string(),
            status: record.status,
            residency_state: ResidencyState::parse("resident").expect("known residency state"),
            created_at: record.created_at,
            updated_at: record.updated_at,
            parent_session_id: record.parent_session_id,
            root_session_id: record.root_session_id,
            depth: record.depth,
            execution_mode: spec.execution_mode.unwrap_or(ExecutionMode::InProcess),
            model: record.model,
            child_session_id,
            resolved_model: record.resolved_model,
            name: record.name,
            category: record.category,
            agent_type: record.agent_type,
        })
    }
}

impl TeamMemberCancelPort for TeamPortManager {
    fn cancel_task(&self, id_or_name: &str, _reason: Option<&str>) -> TeamCancelOutcome {
        TeamCancelOutcome::NotFound {
            reason: format!("unexpected cancel of {id_or_name}"),
        }
    }
}

impl TeamRuntimeManagerPort for TeamPortManager {
    fn start(&self, spec: &TeamMemberStartSpec) -> Result<TeamStartResult, String> {
        let manager_spec = ManagerStartSpec {
            name: spec.name.clone(),
            prompt: spec.prompt.clone(),
            parent_session_id: spec.parent_session_id.clone(),
            depth: spec.depth,
            execution_mode: spec.execution_mode,
            category: spec.category.clone(),
            subagent_type: spec.subagent_type.clone(),
            model: spec.model.clone(),
            ..ManagerStartSpec::default()
        };
        match self.manager.start(&manager_spec) {
            StartResult::Started(task) => {
                let task_id = task.task_id.clone();
                lock(&self.specs).insert(task_id.clone(), spec.clone());
                Ok(TeamStartResult::Started(TeamStartedMember {
                    name: spec.name.clone().unwrap_or_else(|| task_id.clone()),
                    task_id,
                    status: task.status,
                    resolved_model: task.resolved_model,
                }))
            }
            StartResult::StartFailed(failure) => Ok(TeamStartResult::Rejected {
                kind: "error".to_string(),
                reason: failure.error_message,
            }),
            other => Ok(TeamStartResult::Rejected {
                kind: other.kind().to_string(),
                reason: format!("{other:?}"),
            }),
        }
    }

    fn get_resident_handle(&self, task_id: &str) -> Option<ResidentSessionRef> {
        self.manager
            .get_resident_handle(task_id)
            .map(|handle| ResidentSessionRef {
                session_id: handle.session_id(),
            })
    }
}

/// `makeLifecycleManager`: manager + lifecycle wired through a live residency registry.
pub(crate) fn make_lifecycle_manager(
    runner: Arc<dyn ManagedRunner>,
    manager_config: ManagerConfig,
    lifecycle_config: Value,
) -> Harness {
    let project = Project::new();
    let store = project.store();
    let registry = Arc::new(ManagerRegistry::default());
    let lifecycle = Arc::new(create_task_lifecycle(LifecycleDeps::new(
        Arc::new(store.clone()),
        Arc::clone(&registry) as Arc<dyn ResidencyRegistry>,
        lifecycle_settings(lifecycle_config),
    )));
    let lifecycle_slot = Arc::new(Mutex::new(Some(Arc::clone(&lifecycle))));
    let mut options = TaskManagerOptions::new(
        store.clone(),
        ManagedRunners {
            in_process: Arc::clone(&runner),
            process: runner,
        },
        category_planner(&[]),
        project.cwd(),
    );
    options.config = manager_config;
    options.destruction = Some(Arc::new(LifecycleDestruction(lifecycle_slot)));
    let admit_lifecycle = Arc::clone(&lifecycle);
    options.admit = Some(Arc::new(move |parent: &str| {
        match admit_lifecycle.admit_resident(parent) {
            Ok(AdmissionResult::Admitted) => SpawnAdmission::Admitted,
            Ok(AdmissionResult::Evicted { evicted_task_id }) => {
                SpawnAdmission::Evicted { evicted_task_id }
            }
            Ok(AdmissionResult::Rejected(error)) => SpawnAdmission::Rejected {
                message: error.to_string(),
            },
            Err(error) => SpawnAdmission::Rejected {
                message: error.to_string(),
            },
        }
    }));
    let manager = create_task_manager(options);
    *lock(&registry.0) = Some(manager.clone());
    let placeholder = FakeRunner::new();
    Harness {
        manager,
        store,
        in_process: Arc::clone(&placeholder),
        process: placeholder,
        project,
    }
}
