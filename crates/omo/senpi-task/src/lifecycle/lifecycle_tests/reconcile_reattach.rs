//! `lifecycle/reconcile.test.ts`, first `describe` block (`reconcileOnSessionStart reattach`): the
//! cases that drive a real `createTaskManager` (`createHarness` in the TS file) so respawn goes
//! through the manager's registered reattach ports. The cases that only exercise
//! `createTaskLifecycle` (including the second `describe` block, cross-process ownership) live in
//! `reconcile.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::*;
use crate::host::HostError;
use crate::lifecycle::port::{ReattachResult, ResidentKind, RespawnResult};
use crate::lifecycle::reconcile::child_session_dir;
use crate::lifecycle::{LifecycleDeps, ReconcileOutcomeKind, TaskLifecycle, create_task_lifecycle};
use crate::manager::child_handle::{ManagedChildHandle, ManagedChildListener, Unsubscribe};
use crate::manager::execution_mode::ExecutionMode;
use crate::manager::types::{
    ManagedRunner, ManagedRunnerError, ManagedRunnerResult, ManagedRunners, ManagedStartSpec,
    ManagerConfig, ManagerStartSpec, ResolvedChildPlan, RpcRespawnRunner, StartResult,
    TaskManagerOptions, TrustedRespawnLaunch, TrustedRespawnLaunchResolver,
};
use crate::manager::{TaskManager, create_task_manager};
use crate::runners::RunnerOutcome;
use crate::runners::types::{RpcRunnerSpec, RpcSpawnSpec, RpcSwitchSessionResult};
use crate::state::{ResidencyState::*, TaskSpawnSpec, TaskStatus::*};
use crate::team::member_respawn::{
    TeamMemberRespawnLaunchResolverOptions, create_team_member_respawn_launch_resolver,
};
use crate::team::runtime_config::TeamTaskBounds;
use crate::team::runtime_types::TeamMemberExtensionConfig;

const NOW: i64 = 5_000_000;
const MANAGER_CWD: &str = "/tmp/project";
const THIS_PID: i64 = 1111;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

static EVENTS: (Mutex<u64>, Condvar) = (Mutex::new(0), Condvar::new());
const RECHECK: Duration = Duration::from_millis(5);
const WAIT: Duration = Duration::from_secs(5);

fn notify() {
    *lock(&EVENTS.0) += 1;
    EVENTS.1.notify_all();
}

/// Waits for fake-event notifications until `condition` holds; panics with `what` after [`WAIT`].
fn wait_until(what: &str, condition: impl Fn() -> bool) {
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

fn wait_flag(pair: &(Mutex<bool>, Condvar), what: &str) {
    let deadline = Instant::now() + WAIT;
    let mut flag = lock(&pair.0);
    while !*flag {
        let now = Instant::now();
        assert!(now < deadline, "timed out waiting for {what}");
        flag = pair
            .1
            .wait_timeout(flag, deadline - now)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

fn set_flag(pair: &(Mutex<bool>, Condvar), value: bool) {
    *lock(&pair.0) = value;
    pair.1.notify_all();
}

// ---------------------------------------------------------------- `FakeRespawnRunner`

/// `FakeRespawnRunner.controls[i]` in `reconcile.test.ts`.
#[derive(Default)]
struct RespawnControl {
    switch_calls: Mutex<Vec<String>>,
    terminated: AtomicUsize,
    disposed: AtomicUsize,
    settle: Mutex<Option<Sender<RunnerOutcome>>>,
}

impl RespawnControl {
    fn switch_calls(&self) -> Vec<String> {
        lock(&self.switch_calls).clone()
    }

    fn terminated(&self) -> usize {
        self.terminated.load(Ordering::SeqCst)
    }

    fn disposed(&self) -> usize {
        self.disposed.load(Ordering::SeqCst)
    }

    /// `control.settle()`: resolves the respawned child's idle turn.
    fn settle_turn(&self) {
        if let Some(sender) = lock(&self.settle).take() {
            let _ = sender.send(RunnerOutcome::Completed {
                final_response: "reattached result".to_string(),
            });
        }
    }
}

/// The `RpcChildHandle` `FakeRespawnRunner.start` hands back: pid `1000 + started`, a turn that
/// settles only through [`RespawnControl::settle_turn`], and a `switchSession` honouring
/// `cancelSwitch`.
struct RespawnHandle {
    task_id: String,
    pid: i64,
    control: Arc<RespawnControl>,
    cancel_switch: bool,
    outcomes: Mutex<Receiver<RunnerOutcome>>,
}

impl ManagedChildHandle for RespawnHandle {
    fn task_id(&self) -> &str {
        &self.task_id
    }

    fn session_id(&self) -> Option<String> {
        Some(format!("resumed-{}", self.task_id))
    }

    fn pid(&self) -> Option<i64> {
        Some(self.pid)
    }

    fn steer(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn follow_up(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn abort(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn subscribe(&self, _listener: ManagedChildListener) -> Unsubscribe {
        Box::new(|| ())
    }

    fn wait_for_outcome(&self) -> RunnerOutcome {
        lock(&self.outcomes)
            .recv()
            .unwrap_or(RunnerOutcome::Cancelled)
    }

    fn switch_session(
        &self,
        session_path: &str,
    ) -> Option<Result<RpcSwitchSessionResult, HostError>> {
        lock(&self.control.switch_calls).push(session_path.to_string());
        notify();
        Some(Ok(RpcSwitchSessionResult {
            cancelled: self.cancel_switch,
        }))
    }

    fn last_assistant_text(&self) -> Option<String> {
        Some("reattached result".to_string())
    }

    fn has_terminate(&self) -> bool {
        true
    }

    fn terminate(&self) -> Result<(), HostError> {
        self.control.terminated.fetch_add(1, Ordering::SeqCst);
        notify();
        Ok(())
    }

    fn dispose(&self) -> Result<(), HostError> {
        self.control.disposed.fetch_add(1, Ordering::SeqCst);
        notify();
        Ok(())
    }
}

#[derive(Default)]
struct FakeRespawnRunner {
    specs: Mutex<Vec<RpcRunnerSpec>>,
    controls: Mutex<Vec<Arc<RespawnControl>>>,
    cancel_switch: AtomicBool,
}

impl FakeRespawnRunner {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn specs(&self) -> Vec<RpcRunnerSpec> {
        lock(&self.specs).clone()
    }

    fn started_task_ids(&self) -> Vec<String> {
        self.specs().iter().map(|spec| spec.task_id.clone()).collect()
    }

    fn controls(&self) -> Vec<Arc<RespawnControl>> {
        lock(&self.controls).clone()
    }

    fn cancel_switch(&self, value: bool) {
        self.cancel_switch.store(value, Ordering::SeqCst);
    }
}

impl RpcRespawnRunner for FakeRespawnRunner {
    fn start(&self, spec: &RpcRunnerSpec) -> ManagedRunnerResult {
        let pid = {
            let mut specs = lock(&self.specs);
            specs.push(spec.clone());
            1_000 + i64::try_from(specs.len()).unwrap_or(1)
        };
        let control = Arc::new(RespawnControl::default());
        let (settle, outcomes) = channel();
        *lock(&control.settle) = Some(settle);
        lock(&self.controls).push(Arc::clone(&control));
        notify();
        Ok(Arc::new(RespawnHandle {
            task_id: spec.task_id.clone(),
            pid,
            control,
            cancel_switch: self.cancel_switch.load(Ordering::SeqCst),
            outcomes: Mutex::new(outcomes),
        }) as Arc<dyn ManagedChildHandle>)
    }
}

// ---------------------------------------------------------------- managed runner fakes

/// A scripted `ManagedRunner` handle whose turn settles on demand (`FakeHandle` in the TS fixtures).
struct FakeTurnHandle {
    task_id: String,
    pid: Option<i64>,
    outcomes: Mutex<Receiver<RunnerOutcome>>,
    settle: Mutex<Sender<RunnerOutcome>>,
    spawn_spec: Option<RpcSpawnSpec>,
}

impl ManagedChildHandle for FakeTurnHandle {
    fn task_id(&self) -> &str {
        &self.task_id
    }

    fn session_id(&self) -> Option<String> {
        Some(format!("sess-{}", self.task_id))
    }

    fn pid(&self) -> Option<i64> {
        self.pid
    }

    fn spawn_spec(&self) -> Option<RpcSpawnSpec> {
        self.spawn_spec.clone()
    }

    fn steer(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn follow_up(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn abort(&self) -> Result<(), HostError> {
        notify();
        Ok(())
    }

    fn subscribe(&self, _listener: ManagedChildListener) -> Unsubscribe {
        Box::new(|| ())
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
        notify();
        Ok(())
    }
}

#[derive(Default)]
struct FakeManagedRunner {
    specs: Mutex<Vec<ManagedStartSpec>>,
    handles: Mutex<HashMap<String, Arc<FakeTurnHandle>>>,
    started: Condvar,
    /// `EffectiveSpawnRunner`: the launch inputs the runner reports back on the handle.
    reported_spawn_spec: Option<RpcSpawnSpec>,
}

impl FakeManagedRunner {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn with_reported_spawn_spec(spawn_spec: RpcSpawnSpec) -> Arc<Self> {
        Arc::new(Self {
            reported_spawn_spec: Some(spawn_spec),
            ..Self::default()
        })
    }

    fn settle(&self, task_id: &str, outcome: RunnerOutcome) {
        let handle = lock(&self.handles).get(task_id).cloned();
        if let Some(handle) = handle {
            let sender = lock(&handle.settle).clone();
            let _ = sender.send(outcome);
        }
    }

    fn wait_started(&self, task_id: &str) {
        let deadline = Instant::now() + WAIT;
        let mut handles = lock(&self.handles);
        while !handles.contains_key(task_id) {
            let now = Instant::now();
            assert!(now < deadline, "timed out waiting for the {task_id} handle to start");
            handles = self.started
                .wait_timeout(handles, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

impl ManagedRunner for FakeManagedRunner {
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult {
        lock(&self.specs).push(spec.clone());
        let (settle, outcomes) = channel();
        let handle = Arc::new(FakeTurnHandle {
            task_id: spec.task_id.clone(),
            pid: None,
            outcomes: Mutex::new(outcomes),
            settle: Mutex::new(settle),
            spawn_spec: self.reported_spawn_spec.clone(),
        });
        lock(&self.handles).insert(spec.task_id.clone(), Arc::clone(&handle));
        self.started.notify_all();
        notify();
        Ok(handle as Arc<dyn ManagedChildHandle>)
    }
}

/// `injectedHandle` in the "injected reattach ports" case: a plain managed handle with a session id.
struct InjectedHandle {
    task_id: String,
    session_id: String,
    pid: Option<i64>,
}

impl ManagedChildHandle for InjectedHandle {
    fn task_id(&self) -> &str {
        &self.task_id
    }

    fn session_id(&self) -> Option<String> {
        Some(self.session_id.clone())
    }

    fn pid(&self) -> Option<i64> {
        self.pid
    }

    fn steer(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn follow_up(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn abort(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn subscribe(&self, _listener: ManagedChildListener) -> Unsubscribe {
        Box::new(|| ())
    }

    fn wait_for_outcome(&self) -> RunnerOutcome {
        panic!("an injected reattach port never hands the handle to the outcome tracker")
    }

    fn last_assistant_text(&self) -> Option<String> {
        None
    }

    fn dispose(&self) -> Result<(), HostError> {
        Ok(())
    }
}

// ---------------------------------------------------------------- harness

fn planner() -> crate::manager::types::ChildPlanner {
    Arc::new(|_spec: &ManagerStartSpec| {
        Ok(ResolvedChildPlan {
            model: "anthropic/claude".to_string(),
            ..ResolvedChildPlan::default()
        })
    })
}

struct HarnessOptions {
    task_id: &'static str,
    status: TaskStatus,
    mode: &'static str,
    pid: Option<i64>,
    sessions: bool,
    alive: bool,
    config: Value,
    registry: Option<Arc<FakeRegistry>>,
    concurrency: usize,
    host_pid: Option<i64>,
    process_runner: Option<Arc<FakeManagedRunner>>,
    trusted_launch: Option<TrustedRespawnLaunchResolver>,
    spawn_spec: bool,
    name: Option<&'static str>,
}

impl Default for HarnessOptions {
    fn default() -> Self {
        Self {
            task_id: "st_00000001",
            status: Running,
            mode: "process",
            pid: Some(900),
            sessions: true,
            alive: false,
            config: json!({}),
            registry: None,
            concurrency: 5,
            host_pid: None,
            process_runner: None,
            trusted_launch: None,
            spawn_spec: true,
            name: None,
        }
    }
}

struct Harness {
    _temp: Option<TempStore>,
    store: Arc<TaskRecordStore>,
    session_path: Option<PathBuf>,
    respawn: Arc<FakeRespawnRunner>,
    signals: Arc<FakeSignaller>,
    lifecycle: TaskLifecycle,
    manager: TaskManager,
    in_process: Arc<FakeManagedRunner>,
}

/// `seedProcessRecord` in `reconcile.test.ts`: a resident process record carrying the legacy
/// persisted launch inputs.
fn seed_process_record(store: &TaskRecordStore, task_id: &str) -> TaskRecord {
    let record = seed_record(
        store,
        Seed {
            task_id,
            status: Some(Running),
            residency_state: Some(Resident),
            execution_mode: Some("process"),
            pid: Some(900),
            updated_at: Some(crate::shared::iso_from_ms(NOW - 1_000)),
            ..Seed::default()
        },
    );
    let persisted = TaskRecord {
        spawn_spec: Some(TaskSpawnSpec::LegacyProcess {
            cwd: MANAGER_CWD.to_string(),
            extensions: Some(vec!["/tmp/member-extension.ts".to_string()]),
            member_env: Some(vec![(
                "SENPI_TASK_MEMBER".to_string(),
                "run-1::alpha".to_string(),
            )]),
        }),
        ..record
    };
    store.replace(&persisted).expect("replace spawn spec");
    persisted
}

fn set_mtime(path: &Path, ms: u64) {
    let time = std::time::UNIX_EPOCH + Duration::from_millis(ms);
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(time))
        .expect("set mtime");
}

/// `persistSessions`: two transcripts, the `2026-07-12` one newest by mtime.
fn persist_sessions(store: &TaskRecordStore, task_id: &str) -> PathBuf {
    let directory = child_session_dir(store.state_dir(), task_id);
    std::fs::create_dir_all(&directory).expect("session dir");
    let older = directory.join("2026-07-11_old.jsonl");
    let newest = directory.join("2026-07-12_new.jsonl");
    std::fs::write(&older, "{}\n").expect("older transcript");
    std::fs::write(&newest, "{}\n").expect("newest transcript");
    set_mtime(&older, 1_000);
    set_mtime(&newest, 2_000);
    newest
}

/// `createHarness` + `createManager`: a manager registering the lifecycle reattach ports for this
/// store, and a lifecycle resolving them from the same state dir.
fn create_harness(options: HarnessOptions) -> Harness {
    let temp = temp_store();
    let store = Arc::clone(&temp.store);
    let mut record = seed_record(
        &store,
        Seed {
            task_id: options.task_id,
            status: Some(options.status),
            residency_state: Some(Resident),
            execution_mode: Some(options.mode),
            pid: options.pid,
            updated_at: Some(crate::shared::iso_from_ms(NOW - 1_000)),
            host_pid: options.host_pid,
            ..Seed::default()
        },
    );
    if let Some(name) = options.name {
        record = TaskRecord {
            name: Some(name.to_string()),
            ..record
        };
    }
    if options.spawn_spec {
        record = TaskRecord {
            spawn_spec: Some(TaskSpawnSpec::LegacyProcess {
                cwd: MANAGER_CWD.to_string(),
                extensions: Some(vec!["/tmp/member-extension.ts".to_string()]),
                member_env: Some(vec![(
                    "SENPI_TASK_MEMBER".to_string(),
                    "run-1::alpha".to_string(),
                )]),
            }),
            ..record
        };
    }
    store.replace(&record).expect("seed record");

    let session_path = options
        .sessions
        .then(|| persist_sessions(&store, options.task_id));
    build_harness(store, session_path, options, Some(temp))
}

/// The manager + lifecycle pair over a store the caller seeded (the multi-record cases).
fn create_harness_for_store(store: &Arc<TaskRecordStore>, options: HarnessOptions) -> Harness {
    build_harness(Arc::clone(store), None, options, None)
}

fn build_harness(
    store: Arc<TaskRecordStore>,
    session_path: Option<PathBuf>,
    options: HarnessOptions,
    temp: Option<TempStore>,
) -> Harness {
    let respawn = FakeRespawnRunner::new();
    let in_process = FakeManagedRunner::new();
    let process = options.process_runner.unwrap_or_default();
    let mut manager_options = TaskManagerOptions::new(
        (*store).clone(),
        ManagedRunners {
            in_process: Arc::clone(&in_process) as Arc<dyn ManagedRunner>,
            process: Arc::clone(&process) as Arc<dyn ManagedRunner>,
        },
        planner(),
        MANAGER_CWD,
    );
    manager_options.config = ManagerConfig {
        concurrency: crate::manager::concurrency::TaskConcurrencyConfig {
            default_concurrency: Some(options.concurrency),
            provider_concurrency: None,
            model_concurrency: None,
        },
        max_depth: 1,
        default_execution_mode: ExecutionMode::InProcess,
    };
    manager_options.rpc_respawn_runner = Some(Arc::clone(&respawn) as Arc<dyn RpcRespawnRunner>);
    manager_options.trusted_respawn_launch = options.trusted_launch;
    manager_options.host_pid = options.host_pid;
    let manager = create_task_manager(manager_options);

    let registry = options.registry.unwrap_or_default();
    let signals = Arc::new(FakeSignaller {
        dies_on_term: options.alive,
        ..FakeSignaller::with_alive(if options.alive { vec![900] } else { Vec::new() })
    });
    let mut deps = LifecycleDeps::new(
        Arc::clone(&store) as Arc<dyn LifecycleStore>,
        Arc::clone(&registry) as Arc<dyn ResidencyRegistry>,
        settings(options.config.clone()),
    );
    deps.now = Some(Arc::new(|| NOW));
    deps.signaller = Some(Arc::clone(&signals) as Arc<dyn ProcessSignaller>);
    deps.orphan_kill_delay_ms = Some(0);
    deps.host_pid = options.host_pid;
    let lifecycle = create_task_lifecycle(deps);

    Harness {
        _temp: temp,
        store,
        session_path,
        respawn,
        signals,
        lifecycle,
        manager,
        in_process,
    }
}

fn status_of(store: &TaskRecordStore, task_id: &str) -> Option<TaskStatus> {
    store.load(task_id).expect("load").map(|record| record.status)
}

fn started(result: StartResult) -> crate::manager::types::StartedTask {
    match result {
        StartResult::Started(task) => task,
        other => panic!("expected started, got {other:?}"),
    }
}

fn in_process_spec(prompt: &str) -> ManagerStartSpec {
    ManagerStartSpec {
        prompt: prompt.to_string(),
        parent_session_id: "parent-1".to_string(),
        depth: 1,
        execution_mode: Some(ExecutionMode::InProcess),
        ..ManagerStartSpec::default()
    }
}

// ---------------------------------------------------------------- reattach cases

#[test]
fn given_dead_process_and_persisted_sessions_when_reconciled_then_newest_session_respawns_running_at_epoch_plus_one()
 {
    // given
    let harness = create_harness(HarnessOptions {
        task_id: "st_00000001",
        ..HarnessOptions::default()
    });

    // when
    let result = harness
        .lifecycle
        .reconcile_on_session_start(None)
        .expect("reconcile");

    // then
    let session_path = harness
        .session_path
        .clone()
        .expect("expected a persisted session path");
    assert_eq!(result.outcomes[0].kind, ReconcileOutcomeKind::Resumed);
    let specs = harness.respawn.specs();
    assert_eq!(
        specs[0].resume_session_path.as_deref(),
        Some(session_path.to_string_lossy().as_ref())
    );
    assert_eq!(
        harness.respawn.controls()[0].switch_calls(),
        vec![session_path.to_string_lossy().into_owned()]
    );
    let record = harness
        .store
        .load("st_00000001")
        .expect("load")
        .expect("record");
    assert_eq!(record.status, Running);
    assert_eq!(record.notification.run_epoch, 1);
    assert_eq!(
        harness
            .manager
            .get_resident_handle("st_00000001")
            .expect("resident handle")
            .pid(),
        Some(1001)
    );
}

#[test]
fn given_injected_reattach_ports_and_a_registered_store_fallback_when_reconciled_then_the_injected_ports_take_precedence()
 {
    // given
    let harness = create_harness(HarnessOptions {
        task_id: "st_0000000d",
        ..HarnessOptions::default()
    });
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut deps = LifecycleDeps::new(
        Arc::clone(&harness.store) as Arc<dyn LifecycleStore>,
        Arc::new(FakeRegistry::default()),
        settings(json!({})),
    );
    deps.now = Some(Arc::new(|| NOW));
    deps.signaller = Some(Arc::new(FakeSignaller::with_alive(Vec::new())));
    deps.orphan_kill_delay_ms = Some(0);
    let injected = Arc::clone(&calls);
    deps.respawn = Some(Arc::new(move |record: &TaskRecord, path: Option<&Path>| {
        injected.lock().unwrap_or_else(PoisonError::into_inner).push(format!(
            "respawn:{}:{}",
            record.task_id,
            path.expect("session path").display()
        ));
        RespawnResult::Ok(Arc::new(InjectedHandle {
            task_id: record.task_id.clone(),
            session_id: "injected-session".to_string(),
            pid: Some(1001),
        }))
    }));
    let injected = Arc::clone(&calls);
    deps.reattach = Some(Arc::new(
        move |record: &TaskRecord, handle: Arc<dyn ManagedChildHandle>| {
            injected.lock().unwrap_or_else(PoisonError::into_inner).push(format!(
                "reattach:{}:{}",
                record.task_id,
                handle.session_id().unwrap_or_default()
            ));
            ReattachResult::Ok
        },
    ));
    let lifecycle = create_task_lifecycle(deps);

    // when
    let result = lifecycle
        .reconcile_on_session_start(None)
        .expect("reconcile");

    // then
    let session_path = harness
        .session_path
        .clone()
        .expect("expected a persisted session path");
    assert_eq!(result.outcomes[0].kind, ReconcileOutcomeKind::Resumed);
    assert_eq!(
        calls.lock().unwrap_or_else(PoisonError::into_inner).clone(),
        vec![
            format!("respawn:st_0000000d:{}", session_path.display()),
            "reattach:st_0000000d:injected-session".to_string(),
        ]
    );
}

#[test]
fn given_a_dead_process_without_a_session_when_reconciled_then_it_remains_lost_without_respawn() {
    // given
    let harness = create_harness(HarnessOptions {
        task_id: "st_00000002",
        sessions: false,
        ..HarnessOptions::default()
    });

    // when
    let result = harness
        .lifecycle
        .reconcile_on_session_start(None)
        .expect("reconcile");

    // then
    assert_eq!(result.outcomes[0].kind, ReconcileOutcomeKind::Lost);
    let record = harness
        .store
        .load("st_00000002")
        .expect("load")
        .expect("record");
    assert_eq!(record.status, Lost);
    assert_eq!(record.residency_state, Disposed);
    assert!(harness.respawn.specs().is_empty());
}

#[test]
fn given_a_live_foreign_process_and_persisted_session_when_reconciled_then_it_is_terminated_before_respawn_reattach()
 {
    // given
    let harness = create_harness(HarnessOptions {
        task_id: "st_00000003",
        alive: true,
        ..HarnessOptions::default()
    });

    // when
    let result = harness
        .lifecycle
        .reconcile_on_session_start(None)
        .expect("reconcile");

    // then
    assert_eq!(harness.signals.signals(), vec![(900, "SIGTERM")]);
    assert_eq!(harness.respawn.specs().len(), 1);
    assert_eq!(result.outcomes[0].kind, ReconcileOutcomeKind::Resumed);
    let record = harness
        .store
        .load("st_00000003")
        .expect("load")
        .expect("record");
    assert_eq!(record.status, Running);
    assert_eq!(record.residency_state, Resident);
}

#[test]
fn given_an_in_process_record_when_reconciled_then_the_previous_process_task_is_lost_and_never_respawned()
 {
    // given
    let harness = create_harness(HarnessOptions {
        task_id: "st_00000004",
        mode: "in-process",
        pid: None,
        spawn_spec: false,
        ..HarnessOptions::default()
    });

    // when
    let result = harness
        .lifecycle
        .reconcile_on_session_start(None)
        .expect("reconcile");

    // then
    assert_eq!(result.outcomes[0].kind, ReconcileOutcomeKind::Lost);
    assert!(harness.respawn.specs().is_empty());
}

#[test]
fn given_an_orphan_whose_reattach_respawn_then_fails_when_reconciled_then_the_true_terminal_reason_replaces_the_first_lost_reason()
 {
    // given a live orphan child with a persisted session but no usable spawn spec, so the first
    // lost marking (orphan) is followed by a second one (reattach failure) in the same pass
    let task_id = "st_00000014";
    let harness = create_harness(HarnessOptions {
        task_id,
        pid: Some(901),
        alive: true,
        spawn_spec: false,
        ..HarnessOptions::default()
    });

    // when
    let result = harness
        .lifecycle
        .reconcile_on_session_start(None)
        .expect("reconcile");

    // then the reattach failure detail replaces the generic orphan reason instead of being swallowed
    assert_eq!(result.outcomes[0].kind, ReconcileOutcomeKind::Lost);
    let record = harness
        .store
        .load(task_id)
        .expect("load")
        .expect("record");
    assert_eq!(
        record.error_message.as_deref(),
        Some("reattach failed: persisted spawn spec unavailable")
    );
    assert!(read_events(&harness.store, task_id).contains(&"reconcile_lost".to_string()));
}

#[test]
fn given_reconcile_reattach_is_disabled_when_a_durable_session_exists_then_v1_lost_behavior_runs_without_respawn()
 {
    // given
    let harness = create_harness(HarnessOptions {
        task_id: "st_00000005",
        config: json!({ "reattach_on_reconcile": false }),
        ..HarnessOptions::default()
    });

    // when
    let result = harness
        .lifecycle
        .reconcile_on_session_start(None)
        .expect("reconcile");

    // then
    assert_eq!(result.outcomes[0].kind, ReconcileOutcomeKind::Lost);
    assert_eq!(status_of(&harness.store, "st_00000005"), Some(Lost));
    assert!(harness.respawn.specs().is_empty());
}

#[test]
fn given_this_process_already_owns_the_live_handle_when_reconciled_then_the_record_is_skipped_without_signalling_or_respawn()
 {
    // given
    let registry = Arc::new(FakeRegistry::default());
    registry.add(fake_handle(
        "st_00000006",
        ResidentKind::Rpc,
        &call_log(),
        HandleOptions {
            pid: Some(900),
            ..HandleOptions::default()
        },
    ));
    let harness = create_harness(HarnessOptions {
        task_id: "st_00000006",
        alive: true,
        registry: Some(registry),
        ..HarnessOptions::default()
    });

    // when
    let result = harness
        .lifecycle
        .reconcile_on_session_start(None)
        .expect("reconcile");

    // then
    assert_eq!(result.outcomes[0].kind, ReconcileOutcomeKind::Resumed);
    assert_eq!(status_of(&harness.store, "st_00000006"), Some(Running));
    assert!(harness.signals.signals().is_empty());
}

#[test]
fn given_a_completed_resident_daemon_with_a_dead_pid_when_reconciled_then_its_process_returns_while_the_record_stays_terminal()
 {
    // given
    let harness = create_harness(HarnessOptions {
        task_id: "st_00000007",
        status: Completed,
        ..HarnessOptions::default()
    });

    // when
    let result = harness
        .lifecycle
        .reconcile_on_session_start(None)
        .expect("reconcile");

    // then
    assert_eq!(result.outcomes[0].kind, ReconcileOutcomeKind::Resumed);
    let record = harness
        .store
        .load("st_00000007")
        .expect("load")
        .expect("record");
    assert_eq!(record.status, Completed);
    assert_eq!(record.notification.run_epoch, 0);
    assert_eq!(
        harness
            .manager
            .get_resident_handle("st_00000007")
            .expect("resident handle")
            .pid(),
        Some(1001)
    );
}

#[test]
fn given_a_completed_resident_daemon_with_a_live_foreign_pid_when_reconciled_then_it_is_terminated_before_reattach()
 {
    // given
    let harness = create_harness(HarnessOptions {
        task_id: "st_0000000a",
        status: Completed,
        alive: true,
        ..HarnessOptions::default()
    });

    // when
    let result = harness
        .lifecycle
        .reconcile_on_session_start(None)
        .expect("reconcile");

    // then
    assert_eq!(harness.signals.signals(), vec![(900, "SIGTERM")]);
    assert_eq!(result.outcomes[0].kind, ReconcileOutcomeKind::Resumed);
    let record = harness
        .store
        .load("st_0000000a")
        .expect("load")
        .expect("record");
    assert_eq!(record.status, Completed);
    assert_eq!(record.residency_state, Resident);
    assert_eq!(
        harness
            .manager
            .get_resident_handle("st_0000000a")
            .expect("resident handle")
            .pid(),
        Some(1001)
    );
}

#[test]
fn given_overlapping_reconcile_sweeps_when_ownership_claims_race_then_exactly_one_child_respawns_and_the_loser_defers()
 {
    // given
    let harness = create_harness(HarnessOptions {
        task_id: "st_0000000b",
        ..HarnessOptions::default()
    });
    let entered = Arc::new((Mutex::new(false), Condvar::new()));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let manager = harness.manager.clone();
    let respawn_manager = manager.clone();
    let respawn_entered = Arc::clone(&entered);
    let respawn_gate = Arc::clone(&gate);
    let mut deps = LifecycleDeps::new(
        Arc::clone(&harness.store) as Arc<dyn LifecycleStore>,
        Arc::new(FakeRegistry::default()),
        settings(json!({})),
    );
    deps.now = Some(Arc::new(|| NOW));
    deps.signaller = Some(Arc::new(FakeSignaller::with_alive(Vec::new())));
    deps.orphan_kill_delay_ms = Some(0);
    deps.respawn = Some(Arc::new(move |record: &TaskRecord, path: Option<&Path>| {
        set_flag(&respawn_entered, true);
        wait_flag(&respawn_gate, "the racing sweep to be released");
        respawn_manager.respawn(record, path)
    }));
    deps.reattach = Some(Arc::new(move |record: &TaskRecord, handle| {
        manager.reattach(record, handle)
    }));
    let lifecycle = create_task_lifecycle(deps);

    // when
    let (winner, loser) = std::thread::scope(|scope| {
        let racing = scope.spawn(|| {
            lifecycle
                .reconcile_on_session_start(None)
                .expect("first sweep")
        });
        wait_flag(&entered, "the first sweep to reach respawn");
        let loser = lifecycle
            .reconcile_on_session_start(None)
            .expect("second sweep");
        set_flag(&gate, true);
        (racing.join().expect("first sweep"), loser)
    });

    // then
    assert_eq!(harness.respawn.specs().len(), 1);
    let controls = harness.respawn.controls();
    assert_eq!(
        controls
            .iter()
            .map(|control| (control.terminated(), control.disposed()))
            .collect::<Vec<_>>(),
        vec![(0, 0)]
    );
    let mut kinds: Vec<&str> = winner
        .outcomes
        .iter()
        .chain(loser.outcomes.iter())
        .map(|outcome| outcome.kind.as_str())
        .collect();
    kinds.sort_unstable();
    assert_eq!(kinds, vec!["foreign_live_owner", "resumed"]);
    assert_eq!(
        harness
            .store
            .load("st_0000000b")
            .expect("load")
            .expect("record")
            .notification
            .run_epoch,
        1
    );
    assert_eq!(
        harness
            .manager
            .get_resident_handle("st_0000000b")
            .expect("resident handle")
            .pid(),
        Some(1001)
    );
}

#[test]
fn given_a_process_runner_reports_effective_launch_inputs_when_persisted_record_is_reloaded_then_only_safe_spawn_facts_survive()
 {
    // given
    let process_runner = FakeManagedRunner::with_reported_spawn_spec(RpcSpawnSpec {
        cwd: MANAGER_CWD.to_string(),
        extensions: Some(vec!["/tmp/inherited-extension.ts".to_string()]),
        member_env: Some(std::collections::BTreeMap::from([(
            "SENPI_TASK_MEMBER".to_string(),
            "run-1::alpha".to_string(),
        )])),
    });
    let harness = create_harness(HarnessOptions {
        task_id: "st_0000000c",
        process_runner: Some(process_runner),
        ..HarnessOptions::default()
    });

    // when
    let result = harness.manager.start(&ManagerStartSpec {
        prompt: "bootstrap".to_string(),
        parent_session_id: "parent-1".to_string(),
        depth: 1,
        execution_mode: Some(ExecutionMode::Process),
        ..ManagerStartSpec::default()
    });

    // then the v1 rebuild facts persisted at spawn survive, while the runner-reported extensions
    // and member env (untrusted launch inputs) never reach the record
    let task = started(result);
    let record = harness
        .store
        .load(&task.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(
        serde_json::to_value(record.spawn_spec.expect("spawn spec")).expect("json"),
        json!({ "version": 1, "cwd": MANAGER_CWD, "prompt": "bootstrap" })
    );
}

#[test]
fn given_switch_session_is_cancelled_when_reconciled_then_the_fresh_child_is_torn_down_and_the_record_stays_lost()
 {
    // given
    let harness = create_harness(HarnessOptions {
        task_id: "st_00000008",
        ..HarnessOptions::default()
    });
    harness.respawn.cancel_switch(true);

    // when
    let result = harness
        .lifecycle
        .reconcile_on_session_start(None)
        .expect("reconcile");

    // then
    assert_eq!(result.outcomes[0].kind, ReconcileOutcomeKind::Lost);
    assert_eq!(status_of(&harness.store, "st_00000008"), Some(Lost));
    let controls = harness.respawn.controls();
    assert_eq!(controls[0].terminated(), 1);
    assert_eq!(controls[0].disposed(), 1);
    assert!(harness.manager.get_resident_handle("st_00000008").is_none());
}

#[test]
fn given_one_trusted_launch_resolver_rejection_when_reconciling_multiple_process_tasks_then_later_tasks_still_reattach()
 {
    // given
    let temp = temp_store();
    let rejected = seed_process_record(&temp.store, "st_0000000d");
    let healthy = seed_process_record(&temp.store, "st_0000000e");
    persist_sessions(&temp.store, &rejected.task_id);
    persist_sessions(&temp.store, &healthy.task_id);
    let rejected_task_id = rejected.task_id.clone();
    let trusted: TrustedRespawnLaunchResolver = Arc::new(move |record: &TaskRecord| {
        if record.task_id == rejected_task_id {
            Err(ManagedRunnerError::Other(
                "current team runtime unavailable".to_string(),
            ))
        } else {
            Ok(None)
        }
    });
    let harness = create_harness_for_store(
        &temp.store,
        HarnessOptions {
            trusted_launch: Some(trusted),
            ..HarnessOptions::default()
        },
    );

    // when
    let result = harness
        .lifecycle
        .reconcile_on_session_start(None)
        .expect("reconcile");

    // then
    assert_eq!(
        result
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == rejected.task_id)
            .expect("rejected outcome")
            .kind,
        ReconcileOutcomeKind::Lost
    );
    assert_eq!(
        result
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == healthy.task_id)
            .expect("healthy outcome")
            .kind,
        ReconcileOutcomeKind::Resumed
    );
    assert_eq!(harness.respawn.started_task_ids(), vec![healthy.task_id]);
}

#[test]
fn given_a_team_member_whose_current_runtime_is_missing_when_reconciling_then_it_is_lost_without_blocking_later_records()
 {
    // given
    let temp = temp_store();
    let team = seed_process_record(&temp.store, "st_0000000f");
    let team = TaskRecord {
        name: Some("team:11111111-1111-4111-8111-111111111111:alpha".to_string()),
        ..team
    };
    temp.store.replace(&team).expect("replace team record");
    let healthy = seed_process_record(&temp.store, "st_00000010");
    persist_sessions(&temp.store, &team.task_id);
    persist_sessions(&temp.store, &healthy.task_id);
    let resolver = create_team_member_respawn_launch_resolver(TeamMemberRespawnLaunchResolverOptions {
        state_dir: StateDirConfig {
            project_dir: temp.store.state_dir().to_path_buf(),
            task_state_dir: None,
        },
        team_bounds: TeamTaskBounds {
            max_members: 8,
            max_parallel_members: 4,
            max_wall_clock_minutes: 120,
        },
        member_extension: TeamMemberExtensionConfig {
            entry_path: "/trusted/member-extension.js".to_string(),
            inherited_extensions: Some(vec!["/trusted/provider-extension.js".to_string()]),
        },
    })
    .expect("resolver");
    let trusted: TrustedRespawnLaunchResolver = Arc::new(move |record: &TaskRecord| {
        match resolver.resolve(record.name.as_deref(), &record.task_id) {
            Ok(launch) => Ok(Some(TrustedRespawnLaunch {
                extensions: Some(launch.extensions),
                member_env: launch.member_env,
            })),
            Err(error) => Err(ManagedRunnerError::TeamRespawnLaunch {
                code: error.code.as_str().to_string(),
            }),
        }
    });
    let harness = create_harness_for_store(
        &temp.store,
        HarnessOptions {
            trusted_launch: Some(trusted),
            ..HarnessOptions::default()
        },
    );

    // when
    let result = harness
        .lifecycle
        .reconcile_on_session_start(None)
        .expect("reconcile");

    // then
    assert_eq!(
        result
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == team.task_id)
            .expect("team outcome")
            .kind,
        ReconcileOutcomeKind::Lost
    );
    assert_eq!(
        result
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == healthy.task_id)
            .expect("healthy outcome")
            .kind,
        ReconcileOutcomeKind::Resumed
    );
    assert_eq!(harness.respawn.started_task_ids(), vec![healthy.task_id]);
    let specs = harness.respawn.specs();
    assert_eq!(
        specs[0].extensions,
        Some(vec!["/trusted/provider-extension.js".to_string()])
    );
    assert_eq!(specs[0].member_env, None);
}

#[test]
fn given_one_concurrency_slot_and_two_queued_tasks_when_a_reattached_task_completes_then_only_the_first_queued_task_starts()
 {
    // given
    let harness = create_harness(HarnessOptions {
        task_id: "st_00000009",
        concurrency: 1,
        config: json!({ "default_concurrency": 1 }),
        ..HarnessOptions::default()
    });
    harness
        .lifecycle
        .reconcile_on_session_start(None)
        .expect("reconcile");

    // when
    let first_queued = started(harness.manager.start(&in_process_spec("next")));
    let second_queued = started(harness.manager.start(&in_process_spec("later")));
    assert_eq!(first_queued.status, Pending);
    assert_eq!(second_queued.status, Pending);
    harness.respawn.controls()[0].settle_turn();
    wait_until("the reattached task to complete", || {
        status_of(&harness.store, "st_00000009") == Some(Completed)
    });
    harness.in_process.wait_started(&first_queued.task_id);

    // then
    assert_eq!(status_of(&harness.store, &first_queued.task_id), Some(Running));
    assert_eq!(status_of(&harness.store, &second_queued.task_id), Some(Pending));
    harness.in_process.settle(
        &first_queued.task_id,
        RunnerOutcome::Completed {
            final_response: "done".to_string(),
        },
    );
    harness.in_process.wait_started(&second_queued.task_id);
    assert_eq!(status_of(&harness.store, &second_queued.task_id), Some(Running));
}

#[test]
fn given_a_process_mode_record_owned_by_this_process_when_reconciled_then_it_is_not_skipped_as_foreign_and_reattaches_as_before()
 {
    // given a record this process created whose handle is gone; host_pid === hostPid means the
    // foreign-owner guard must NOT skip it and the normal orphan kill + respawn reattach runs
    let harness = create_harness(HarnessOptions {
        task_id: "st_00000023",
        alive: true,
        host_pid: Some(THIS_PID),
        ..HarnessOptions::default()
    });

    // when
    let result = harness
        .lifecycle
        .reconcile_on_session_start(None)
        .expect("reconcile");

    // then
    let session_path = harness
        .session_path
        .clone()
        .expect("expected a persisted session path");
    assert_eq!(result.outcomes[0].kind, ReconcileOutcomeKind::Resumed);
    assert_eq!(
        harness.respawn.controls()[0].switch_calls(),
        vec![session_path.to_string_lossy().into_owned()]
    );
    assert_eq!(harness.signals.signals(), vec![(900, "SIGTERM")]);
    assert_eq!(status_of(&harness.store, "st_00000023"), Some(Running));
}
