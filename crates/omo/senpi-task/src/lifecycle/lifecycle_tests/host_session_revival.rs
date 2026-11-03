use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::*;
use crate::host::HostError;
use crate::lifecycle::host_session::{
    HostEndpointEnsure, HostEndpointNotice, HostEndpointPort,
    HostSessionLiveness, HostSessionProbe, HostSessionRetryPolicy, NoHostEndpoint,
    canonical_session_path,
};
use crate::lifecycle::host_session_revive::{HostSessionParkOptions, HostSessionParkOutcome};
use crate::manager::child_handle::{ManagedChildHandle, ManagedChildListener, Unsubscribe};
use crate::lifecycle::port::{ReattachResult, RespawnResult};
use crate::lifecycle::{LifecycleDeps, ReconcileOutcomeKind, TaskLifecycle, create_task_lifecycle};
use crate::runners::RunnerOutcome;
use crate::state::{
    HostSessionIdentity, ResidencyState, RunnerKind, SuspensionReason, TaskRecord, TaskStatus, TaskTransition,
};
use crate::store::TaskRecordStore;

const HOST_PID: i64 = 4242;
const HOST_SOCKET: &str = "/tmp/dh-fake/rpc.sock";
const OTHER_SHARD: &str = "/tmp/dh-t7/shards/p-00000000000000bb.sock";
const OWN_SHARD: &str = "/tmp/dh-t7/shards/p-00000000000000aa.sock";

struct TestHandle {
    task_id: String,
}

impl ManagedChildHandle for TestHandle {
    fn task_id(&self) -> &str {
        &self.task_id
    }
    fn session_id(&self) -> Option<String> {
        Some("child".to_string())
    }
    fn pid(&self) -> Option<i64> {
        None
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
        Box::new(|| {})
    }
    fn wait_for_outcome(&self) -> RunnerOutcome {
        panic!("test handles do not wait for outcome in lifecycle tests");
    }
    fn last_assistant_text(&self) -> Option<String> {
        None
    }
    fn dispose(&self) -> Result<(), HostError> {
        Ok(())
    }
}

struct FakeDaemon {
    alive: AtomicBool,
    live_paths: Mutex<HashSet<String>>,
    probe_calls: AtomicUsize,
    list_calls: AtomicUsize,
}

impl FakeDaemon {
    fn new(alive: bool) -> Self {
        Self {
            alive: AtomicBool::new(alive),
            live_paths: Mutex::default(),
            probe_calls: AtomicUsize::default(),
            list_calls: AtomicUsize::default(),
        }
    }

    fn hold(&self, path: &str) {
        self.live_paths
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(canonical_session_path(path));
    }
}

struct FakeProbe {
    daemon: Arc<FakeDaemon>,
    socket: String,
    refreshes: Mutex<Vec<Option<String>>>,
    probed_sockets: Mutex<Vec<String>>,
}

impl FakeProbe {
    fn new(daemon: Arc<FakeDaemon>, socket: &str) -> Self {
        Self {
            daemon,
            socket: socket.to_string(),
            refreshes: Mutex::default(),
            probed_sockets: Mutex::default(),
        }
    }
}

impl HostSessionProbe for FakeProbe {
    fn daemon_alive(&self, host_session: &HostSessionIdentity) -> bool {
        self.probed_sockets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(host_session.socket.clone());
        self.daemon.probe_calls.fetch_add(1, Ordering::SeqCst);
        self.daemon.alive.load(Ordering::SeqCst) && host_session.socket == self.socket
    }

    fn session_live(&self, host_session: &HostSessionIdentity) -> bool {
        self.daemon.list_calls.fetch_add(1, Ordering::SeqCst);
        self.daemon
            .live_paths
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&canonical_session_path(&host_session.session_path))
    }

    fn session_liveness(&self, host_session: &HostSessionIdentity) -> HostSessionLiveness {
        if self.session_live(host_session) {
            HostSessionLiveness::Live
        } else {
            HostSessionLiveness::Gone
        }
    }

    fn refresh(&self, socket: Option<&str>) {
        self.refreshes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(socket.map(str::to_string));
    }
}

struct FakeEndpoint {
    own_socket: Option<String>,
    answering: Mutex<HashSet<String>>,
    incompatible: Mutex<HashSet<String>>,
    ensures: Mutex<Vec<String>>,
    notices: Mutex<Vec<String>>,
}

impl FakeEndpoint {
    fn new(own_socket: Option<&str>) -> Self {
        Self {
            own_socket: own_socket.map(str::to_string),
            answering: Mutex::default(),
            incompatible: Mutex::default(),
            ensures: Mutex::default(),
            notices: Mutex::default(),
        }
    }
}

impl HostEndpointPort for FakeEndpoint {
    fn is_own(&self, socket: &str) -> bool {
        self.own_socket.as_deref() == Some(socket)
    }

    fn ensure(&self, socket: &str) -> HostEndpointEnsure {
        self.ensures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(socket.to_string());
        if self
            .incompatible
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(socket)
        {
            return HostEndpointEnsure::Incompatible;
        }
        self.answering
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(socket.to_string());
        HostEndpointEnsure::Ensured
    }

    fn notice(&self, reason: HostEndpointNotice, socket: &str) {
        let entry = format!("{}:{}", reason.as_str(), socket);
        let mut notices = self.notices.lock().unwrap_or_else(PoisonError::into_inner);
        if !notices.contains(&entry) {
            notices.push(entry);
        }
    }
}

fn host_identity(task_id: &str, socket: Option<&str>) -> HostSessionIdentity {
    HostSessionIdentity {
        socket: socket.unwrap_or(HOST_SOCKET).to_string(),
        routing_id: format!("routing-{task_id}"),
        session_path: format!("/tmp/dh-fake/sessions/{task_id}.jsonl"),
        instance_id: "instance-1".to_string(),
        daemon_pid: None,
        effect_seq: None,
        adopted_entry_id: None,
    }
}

fn seed_host_task(
    store: &TaskRecordStore,
    task_id: &str,
    parent: &str,
    status: TaskStatus,
    residency: ResidencyState,
    host_pid: Option<i64>,
    identity: HostSessionIdentity,
) -> TaskRecord {
    seed_record(
        store,
        Seed {
            task_id,
            parent_session_id: Some(parent),
            status: Some(status),
            residency_state: Some(residency),
            execution_mode: Some("process"),
            host_pid,
            runner_kind: Some(RunnerKind::HostSession),
            host_session: Some(identity),
            ..Seed::default()
        },
    )
}

#[derive(Clone)]
struct RespawnEntry {
    task_id: String,
    session_path: Option<String>,
}

type OnWaitFn = Arc<dyn Fn(u64) + Send + Sync>;
type OnRespawnFn = Arc<dyn Fn(&str, Option<&str>) + Send + Sync>;

struct FixtureOptions {
    alive: bool,
    host_pid: i64,
    is_alive_pids: Vec<i64>,
    daemon_loss_backoffs: Vec<u64>,
    deferred_retry_backoffs: Vec<u64>,
    on_wait: Option<OnWaitFn>,
    endpoint: Option<Arc<dyn HostEndpointPort>>,
    probe: Option<Arc<dyn HostSessionProbe>>,
    on_respawn: Option<OnRespawnFn>,
}

impl Default for FixtureOptions {
    fn default() -> Self {
        Self {
            alive: true,
            host_pid: HOST_PID,
            is_alive_pids: vec![HOST_PID],
            daemon_loss_backoffs: vec![1_000, 4_000, 16_000],
            deferred_retry_backoffs: Vec::new(),
            on_wait: None,
            endpoint: None,
            probe: None,
            on_respawn: None,
        }
    }
}

struct Fixture {
    daemon: Arc<FakeDaemon>,
    waits: Arc<Mutex<Vec<u64>>>,
    respawned: Arc<Mutex<Vec<RespawnEntry>>>,
    signaller: Arc<FakeSignaller>,
    lifecycle: TaskLifecycle,
}

fn build_fixture(store: Arc<TaskRecordStore>, options: FixtureOptions) -> Fixture {
    let daemon = Arc::new(FakeDaemon::new(options.alive));
    let probe = Arc::new(FakeProbe::new(daemon.clone(), HOST_SOCKET));
    let probe_for_deps: Arc<dyn HostSessionProbe> = options.probe.unwrap_or_else(|| probe.clone());
    let signaller = FakeSignaller::alive(options.is_alive_pids);
    let waits = Arc::new(Mutex::new(Vec::new()));
    let respawned = Arc::new(Mutex::new(Vec::new()));

    let waits_for_retry = waits.clone();
    let on_wait_hook = options.on_wait;
    let host_retry_wait = Arc::new(move |ms: u64| {
        waits_for_retry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(ms);
        if let Some(ref hook) = on_wait_hook {
            hook(ms);
        }
    });

    let respawned_for_deps = respawned.clone();
    let on_respawn_hook = options.on_respawn;
    let respawn = Arc::new(
        move |record: &TaskRecord, path: Option<&std::path::Path>| {
            let path_str = path.map(|p| p.to_string_lossy().into_owned());
            respawned_for_deps
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(RespawnEntry {
                    task_id: record.task_id.clone(),
                    session_path: path_str.clone(),
                });
            if let Some(ref hook) = on_respawn_hook {
                hook(&record.task_id, path_str.as_deref());
            }
            RespawnResult::Ok(Arc::new(TestHandle {
                task_id: record.task_id.clone(),
            }))
        },
    );

    let mut deps = LifecycleDeps::new(
        store,
        Arc::new(FakeRegistry::default()),
        default_settings(),
    );
    deps.host_pid = Some(options.host_pid);
    deps.signaller = Some(signaller.clone());
    deps.orphan_kill_delay_ms = Some(0);
    deps.host_session_probe = Some(probe_for_deps);
    deps.host_endpoint = options.endpoint.or_else(|| Some(Arc::new(NoHostEndpoint)));
    deps.respawn = Some(respawn);
    deps.reattach = Some(Arc::new(|_, _| ReattachResult::Ok));
    deps.host_retry = Some(HostSessionRetryPolicy {
        max_drain_attempts: 10,
        default_retry_after_ms: 2_000,
        daemon_loss_backoff_ms: options.daemon_loss_backoffs,
        deferred_retry_backoff_ms: options.deferred_retry_backoffs,
    });
    deps.host_retry_wait = Some(host_retry_wait);
    deps.live_parent_recovery =
        Some(crate::lifecycle::live_parent_recovery::RecoverySchedule::Disabled);

    let lifecycle = create_task_lifecycle(deps);
    Fixture {
        daemon,
        waits,
        respawned,
        signaller,
        lifecycle,
    }
}

#[test]
fn live_host_session_owned_by_other_session_deferred_without_respawn() {
    let temp = temp_store();
    let identity = host_identity("st_0b000001", None);
    let fixture = build_fixture(temp.store.clone(), FixtureOptions::default());
    fixture.daemon.hold(&identity.session_path);
    seed_host_task(
        &temp.store,
        "st_0b000001",
        "other-session",
        TaskStatus::Running,
        ResidencyState::Resident,
        Some(9999),
        identity,
    );
    let result = fixture
        .lifecycle
        .reconcile_on_session_start(Some("parent-1"))
        .expect("reconcile");
    let record = temp.store.load("st_0b000001").expect("load").expect("record");
    assert_ne!(record.status, TaskStatus::Lost);
    assert!(result.outcomes.iter().any(|o| o.task_id == "st_0b000001"
        && o.kind == ReconcileOutcomeKind::Deferred
        && o.reason.as_deref() == Some("foreign_live_owner")));
    assert_eq!(fixture.respawned.lock().unwrap().len(), 0);
}

#[test]
fn own_live_host_session_whose_host_died_reclaims_and_resumes() {
    let temp = temp_store();
    let identity = host_identity("st_0b00000a", None);
    let fixture = build_fixture(
        temp.store.clone(),
        FixtureOptions {
            is_alive_pids: vec![HOST_PID],
            ..FixtureOptions::default()
        },
    );
    fixture.daemon.hold(&identity.session_path);
    seed_host_task(
        &temp.store,
        "st_0b00000a",
        "parent-1",
        TaskStatus::Running,
        ResidencyState::Resident,
        Some(9999),
        identity.clone(),
    );
    let result = fixture
        .lifecycle
        .reconcile_on_session_start(Some("parent-1"))
        .expect("reconcile");
    assert!(result.outcomes.iter().any(|o| o.task_id == "st_0b00000a"
        && o.kind == ReconcileOutcomeKind::Resumed
        && o.reason.as_deref() == Some("respawned and reattached")));
    let respawned = fixture.respawned.lock().unwrap().clone();
    assert_eq!(respawned.len(), 1);
    assert_eq!(respawned[0].task_id, "st_0b00000a");
    assert_eq!(
        respawned[0].session_path.as_deref(),
        Some(identity.session_path.as_str())
    );
    let record = temp.store.load("st_0b00000a").expect("load").expect("record");
    assert_eq!(record.host_pid, Some(HOST_PID));
    assert_eq!(record.residency_state, ResidencyState::Resident);
}

#[test]
fn own_live_host_session_whose_host_alive_deferred() {
    let temp = temp_store();
    let identity = host_identity("st_0b00000b", None);
    let fixture = build_fixture(
        temp.store.clone(),
        FixtureOptions {
            is_alive_pids: vec![HOST_PID, 9999],
            ..FixtureOptions::default()
        },
    );
    fixture.daemon.hold(&identity.session_path);
    seed_host_task(
        &temp.store,
        "st_0b00000b",
        "parent-1",
        TaskStatus::Running,
        ResidencyState::Resident,
        Some(9999),
        identity,
    );
    let result = fixture
        .lifecycle
        .reconcile_on_session_start(Some("parent-1"))
        .expect("reconcile");
    assert!(result.outcomes.iter().any(|o| o.task_id == "st_0b00000b"
        && o.kind == ReconcileOutcomeKind::Deferred
        && o.reason.as_deref() == Some("foreign_live_owner")));
    assert_eq!(fixture.respawned.lock().unwrap().len(), 0);
}

#[test]
fn parked_host_session_resumes_recorded_path() {
    let temp = temp_store();
    let identity = host_identity("st_0b000002", None);
    let fixture = build_fixture(temp.store.clone(), FixtureOptions::default());
    seed_host_task(
        &temp.store,
        "st_0b000002",
        "parent-1",
        TaskStatus::Running,
        ResidencyState::RpcDetached,
        None,
        identity.clone(),
    );
    let result = fixture
        .lifecycle
        .reconcile_on_session_start(Some("parent-1"))
        .expect("reconcile");
    assert!(result.outcomes.iter().any(|o| o.task_id == "st_0b000002"
        && o.kind == ReconcileOutcomeKind::Resumed));
    let respawned = fixture.respawned.lock().unwrap().clone();
    assert_eq!(respawned.len(), 1);
    assert_eq!(respawned[0].task_id, "st_0b000002");
    assert_eq!(
        respawned[0].session_path.as_deref(),
        Some(identity.session_path.as_str())
    );
    let record = temp.store.load("st_0b000002").expect("load").expect("record");
    assert_ne!(record.status, TaskStatus::Lost);
}

#[test]
fn daemon_loss_bounded_reconcile_stays_rpc_detached() {
    let temp = temp_store();
    let identity = host_identity("st_0b000005", None);
    let fixture = build_fixture(
        temp.store.clone(),
        FixtureOptions {
            alive: false,
            ..FixtureOptions::default()
        },
    );
    seed_host_task(
        &temp.store,
        "st_0b000005",
        "parent-1",
        TaskStatus::Running,
        ResidencyState::Resident,
        Some(HOST_PID),
        identity,
    );
    let outcome = fixture
        .lifecycle
        .park_host_session_on_daemon_loss("st_0b000005")
        .expect("park");
    assert_eq!(
        outcome,
        HostSessionParkOutcome::Suspended {
            reason: SuspensionReason::DaemonUnavailable,
        }
    );
    assert_eq!(*fixture.waits.lock().unwrap(), vec![1_000, 4_000, 16_000]);
    let record = temp.store.load("st_0b000005").expect("load").expect("record");
    assert_eq!(record.residency_state, ResidencyState::RpcDetached);
    assert_eq!(record.status, TaskStatus::Running);
    assert_eq!(
        record.suspension_reason,
        Some(SuspensionReason::DaemonUnavailable)
    );
    assert_eq!(fixture.signaller.signals().len(), 0);
}

#[test]
fn daemon_returns_during_bounded_reconcile_revived() {
    let temp = temp_store();
    let identity = host_identity("st_0b000006", None);
    let fixture = build_fixture(
        temp.store.clone(),
        FixtureOptions {
            alive: false,
            ..FixtureOptions::default()
        },
    );
    seed_host_task(
        &temp.store,
        "st_0b000006",
        "parent-1",
        TaskStatus::Running,
        ResidencyState::Resident,
        Some(HOST_PID),
        identity.clone(),
    );
    let daemon = fixture.daemon.clone();
    let options = HostSessionParkOptions::with_before_attempt(move |attempt| {
        if attempt == 2 {
            daemon.alive.store(true, Ordering::SeqCst);
        }
    });
    let outcome = fixture
        .lifecycle
        .park_host_session_on_daemon_loss_with_options("st_0b000006", options)
        .expect("park");
    assert_eq!(outcome, HostSessionParkOutcome::Revived);
    assert_eq!(*fixture.waits.lock().unwrap(), vec![1_000, 4_000]);
    let respawned = fixture.respawned.lock().unwrap().clone();
    assert_eq!(respawned.len(), 1);
    assert_eq!(respawned[0].task_id, "st_0b000006");
    assert_eq!(
        respawned[0].session_path.as_deref(),
        Some(identity.session_path.as_str())
    );
    let record = temp.store.load("st_0b000006").expect("load").expect("record");
    assert_eq!(record.suspension_reason, None);
}

#[test]
fn daemon_loss_non_host_session_record_suspended() {
    let temp = temp_store();
    seed_record(
        &temp.store,
        Seed {
            task_id: "st_0b000007",
            parent_session_id: Some("parent-1"),
            status: Some(TaskStatus::Running),
            residency_state: Some(ResidencyState::Resident),
            runner_kind: Some(RunnerKind::ChildProcess),
            ..Seed::default()
        },
    );
    let fixture = build_fixture(temp.store.clone(), FixtureOptions::default());
    let outcome = fixture
        .lifecycle
        .park_host_session_on_daemon_loss("st_0b000007")
        .expect("park");
    assert_eq!(
        outcome,
        HostSessionParkOutcome::Suspended {
            reason: SuspensionReason::DaemonUnavailable,
        }
    );
}

#[test]
fn daemon_loss_incompatible_host_stops_retrying() {
    let temp = temp_store();
    let identity = host_identity("st_0c000023", Some(OTHER_SHARD));
    let endpoint = Arc::new(FakeEndpoint::new(Some(OWN_SHARD)));
    endpoint
        .incompatible
        .lock()
        .unwrap()
        .insert(OTHER_SHARD.to_string());
    let fixture = build_fixture(
        temp.store.clone(),
        FixtureOptions {
            alive: false,
            endpoint: Some(endpoint.clone()),
            ..FixtureOptions::default()
        },
    );
    seed_host_task(
        &temp.store,
        "st_0c000023",
        "parent-1",
        TaskStatus::Running,
        ResidencyState::Resident,
        Some(HOST_PID),
        identity,
    );
    let outcome = fixture
        .lifecycle
        .park_host_session_on_daemon_loss("st_0c000023")
        .expect("park");
    assert_eq!(
        outcome,
        HostSessionParkOutcome::Suspended {
            reason: SuspensionReason::HostIncompatible,
        }
    );
    assert_eq!(*fixture.waits.lock().unwrap(), vec![1_000]);
    assert_eq!(fixture.respawned.lock().unwrap().len(), 0);
    assert_eq!(
        *endpoint.ensures.lock().unwrap(),
        vec![OTHER_SHARD.to_string()]
    );
    let record = temp.store.load("st_0c000023").expect("load").expect("record");
    assert_eq!(
        record.suspension_reason,
        Some(SuspensionReason::HostIncompatible)
    );
}

#[test]
fn daemon_loss_own_shard_silent_stays_suspended_own_host_unreachable() {
    let temp = temp_store();
    let identity = host_identity("st_0c000021", Some(OWN_SHARD));
    let endpoint = Arc::new(FakeEndpoint::new(Some(OWN_SHARD)));
    let fixture = build_fixture(
        temp.store.clone(),
        FixtureOptions {
            alive: false,
            endpoint: Some(endpoint.clone()),
            ..FixtureOptions::default()
        },
    );
    seed_host_task(
        &temp.store,
        "st_0c000021",
        "parent-1",
        TaskStatus::Running,
        ResidencyState::Resident,
        Some(HOST_PID),
        identity,
    );
    let outcome = fixture
        .lifecycle
        .park_host_session_on_daemon_loss("st_0c000021")
        .expect("park");
    assert_eq!(
        outcome,
        HostSessionParkOutcome::Suspended {
            reason: SuspensionReason::OwnHostUnreachable,
        }
    );
    assert_eq!(endpoint.ensures.lock().unwrap().len(), 0);
    assert_eq!(fixture.respawned.lock().unwrap().len(), 0);
    let record = temp.store.load("st_0c000021").expect("load").expect("record");
    assert_eq!(
        record.suspension_reason,
        Some(SuspensionReason::OwnHostUnreachable)
    );
    assert_eq!(
        *endpoint.notices.lock().unwrap(),
        vec![format!("own_host_unreachable:{}", OWN_SHARD)]
    );
}

#[test]
fn daemon_loss_different_shard_ensured_once_and_revives() {
    let temp = temp_store();
    let identity = host_identity("st_0c000022", Some(OTHER_SHARD));
    let endpoint = Arc::new(FakeEndpoint::new(Some(OWN_SHARD)));
    let daemon = Arc::new(FakeDaemon::new(false));
    let probe = Arc::new(FakeProbe::new(daemon.clone(), OTHER_SHARD));
    struct EnsuringEndpoint {
        inner: Arc<FakeEndpoint>,
        daemon: Arc<FakeDaemon>,
    }
    impl HostEndpointPort for EnsuringEndpoint {
        fn is_own(&self, socket: &str) -> bool {
            self.inner.is_own(socket)
        }
        fn ensure(&self, socket: &str) -> HostEndpointEnsure {
            let res = self.inner.ensure(socket);
            if res == HostEndpointEnsure::Ensured {
                self.daemon.alive.store(true, Ordering::SeqCst);
            }
            res
        }
        fn notice(&self, reason: HostEndpointNotice, socket: &str) {
            self.inner.notice(reason, socket);
        }
    }
    let combined_endpoint: Arc<dyn HostEndpointPort> = Arc::new(EnsuringEndpoint {
        inner: endpoint.clone(),
        daemon: daemon.clone(),
    });
    let fixture = build_fixture(
        temp.store.clone(),
        FixtureOptions {
            alive: false,
            endpoint: Some(combined_endpoint),
            probe: Some(probe),
            ..FixtureOptions::default()
        },
    );
    seed_host_task(
        &temp.store,
        "st_0c000022",
        "parent-1",
        TaskStatus::Running,
        ResidencyState::Resident,
        Some(HOST_PID),
        identity,
    );
    let outcome = fixture
        .lifecycle
        .park_host_session_on_daemon_loss("st_0c000022")
        .expect("park");
    assert_eq!(outcome, HostSessionParkOutcome::Revived);
    assert_eq!(
        *endpoint.ensures.lock().unwrap(),
        vec![OTHER_SHARD.to_string()]
    );
    let respawned = fixture.respawned.lock().unwrap().clone();
    assert_eq!(respawned.len(), 1);
    assert_eq!(respawned[0].task_id, "st_0c000022");
}

#[test]
fn deferred_child_retried_in_background_when_daemon_answers() {
    let temp = temp_store();
    let identity = host_identity("st_0b000007", None);
    let (tx, rx) = channel();
    let daemon_holder: Arc<Mutex<Option<Arc<FakeDaemon>>>> = Arc::new(Mutex::new(None));
    let daemon_holder_for_wait = daemon_holder.clone();
    let on_wait: OnWaitFn = Arc::new(move |ms| {
        if ms == 15_000
            && let Some(daemon) = daemon_holder_for_wait.lock().unwrap().as_ref() {
            daemon.alive.store(true, Ordering::SeqCst);
        }
    });
    let on_respawn = Arc::new(move |_task_id: &str, _path: Option<&str>| {
        let _ = tx.send(());
    });
    let fixture = build_fixture(
        temp.store.clone(),
        FixtureOptions {
            alive: false,
            deferred_retry_backoffs: vec![5_000, 15_000, 30_000],
            on_wait: Some(on_wait),
            on_respawn: Some(on_respawn),
            ..FixtureOptions::default()
        },
    );
    *daemon_holder.lock().unwrap() = Some(fixture.daemon.clone());
    seed_host_task(
        &temp.store,
        "st_0b000007",
        "parent-1",
        TaskStatus::Running,
        ResidencyState::RpcDetached,
        None,
        identity.clone(),
    );
    let result = fixture
        .lifecycle
        .reconcile_on_session_start(Some("parent-1"))
        .expect("reconcile");
    assert!(result.outcomes.iter().any(|o| o.task_id == "st_0b000007"
        && o.kind == ReconcileOutcomeKind::Deferred
        && o.reason.as_deref() == Some("host_unreachable")));
    rx.recv_timeout(Duration::from_secs(5))
        .expect("retry completed within timeout");
    fixture.lifecycle.context().scoped_retries.wait_until_idle();
    let respawned = fixture.respawned.lock().unwrap().clone();
    assert_eq!(respawned.len(), 1);
    assert_eq!(respawned[0].task_id, "st_0b000007");
    assert_eq!(
        respawned[0].session_path.as_deref(),
        Some(identity.session_path.as_str())
    );
    assert_eq!(*fixture.waits.lock().unwrap(), vec![5_000, 15_000]);
}

#[test]
fn deferred_child_cancelled_meanwhile_nothing_respawned() {
    let temp = temp_store();
    let identity = host_identity("st_0b000008", None);
    let store_for_wait = temp.store.clone();
    let (tx, rx) = channel();
    let daemon_holder: Arc<Mutex<Option<Arc<FakeDaemon>>>> = Arc::new(Mutex::new(None));
    let daemon_holder_for_wait = daemon_holder.clone();
    let on_wait: OnWaitFn = Arc::new(move |_ms| {
        store_for_wait.transition(
            "st_0b000008",
            &TaskTransition::Cancel {
                timestamp: crate::shared::iso_from_ms(1_000),
                error_message: Some("test cancel".to_string()),
                run_stats: None,
            },
        ).expect("cancel during retry");
        if let Some(daemon) = daemon_holder_for_wait.lock().unwrap().as_ref() {
            daemon.alive.store(true, Ordering::SeqCst);
        }
        let _ = tx.send(());
    });
    let fixture = build_fixture(
        temp.store.clone(),
        FixtureOptions {
            alive: false,
            deferred_retry_backoffs: vec![5_000],
            on_wait: Some(on_wait),
            ..FixtureOptions::default()
        },
    );
    *daemon_holder.lock().unwrap() = Some(fixture.daemon.clone());
    seed_host_task(
        &temp.store,
        "st_0b000008",
        "parent-1",
        TaskStatus::Running,
        ResidencyState::RpcDetached,
        None,
        identity,
    );
    let result = fixture
        .lifecycle
        .reconcile_on_session_start(Some("parent-1"))
        .expect("reconcile");
    assert!(result.outcomes.iter().any(|o| o.task_id == "st_0b000008"
        && o.kind == ReconcileOutcomeKind::Deferred));
    rx.recv_timeout(Duration::from_secs(5))
        .expect("wait callback reached");
    fixture.lifecycle.context().scoped_retries.wait_until_idle();
    assert_eq!(fixture.respawned.lock().unwrap().len(), 0);
    let record = temp.store.load("st_0b000008").expect("load").expect("record");
    assert_eq!(record.status, TaskStatus::Cancelled);
}

#[test]
fn singleflight_coordination_prevents_duplicate_retries() {
    let temp = temp_store();
    let fixture = build_fixture(temp.store.clone(), FixtureOptions::default());
    let retries = &fixture.lifecycle.context().scoped_retries;
    assert!(retries.try_start_retry("st_dup_1"));
    assert!(!retries.try_start_retry("st_dup_1"));
    retries.finish_retry("st_dup_1");
    assert!(retries.try_start_retry("st_dup_1"));
    retries.finish_retry("st_dup_1");
}
