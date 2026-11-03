//! `lifecycle/reconcile-startup.test.ts`: the startup reconcile runs under a generation fence. A
//! probe, reach or respawn that returns after dispose, shutdown or a newer ownership claim can
//! neither claim nor revive, and one hung host never holds up another host's records.
//!
//! The upstream awaits become blocking port calls here: each test drives the pass on a thread,
//! waits on the port's entry signal, changes the world, then opens the port's gate.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use super::*;
use crate::lifecycle::errors::LifecycleError;
use crate::lifecycle::port::{
    ReattachResult, RespawnDisposition, RespawnFailureCode, RespawnResult,
};
use crate::lifecycle::{
    HostSessionLiveness, HostSessionProbe, LifecycleDeps, ReconcileOutcomeKind, ReconcileResult,
    SuspendInput, create_task_lifecycle,
};
use crate::manager::child_handle::{ManagedChildHandle, ManagedChildListener, Unsubscribe};
use crate::runners::RunnerOutcome;
use crate::state::{HostSessionIdentity, RunnerKind};

const HOST_PID: i64 = 2222;
const DEAD_OWNER: i64 = 1111;
const SIGNAL_DEADLINE: Duration = Duration::from_secs(10);

/// A one-shot gate a blocked port waits on; every later call sees the opened value at once.
#[derive(Default)]
struct Gate {
    value: Mutex<Option<bool>>,
    opened: Condvar,
}

impl Gate {
    fn open(&self, value: bool) {
        *self.value.lock().unwrap_or_else(PoisonError::into_inner) = Some(value);
        self.opened.notify_all();
    }

    fn wait(&self) -> bool {
        let mut value = self.value.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(opened) = *value {
                return opened;
            }
            value = self
                .opened
                .wait(value)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Signals the first entry, then holds `daemonAlive` for the gated socket (all sockets when
/// `gated_socket` is `None`) until the gate opens. Other sockets answer alive at once.
struct GatedProbe {
    gated_socket: Option<String>,
    entered: Mutex<Option<Sender<()>>>,
    gate: Arc<Gate>,
}

impl GatedProbe {
    fn new(gated_socket: Option<&str>) -> (Arc<Self>, Receiver<()>) {
        let (sender, receiver) = channel();
        let probe = Arc::new(Self {
            gated_socket: gated_socket.map(str::to_string),
            entered: Mutex::new(Some(sender)),
            gate: Arc::default(),
        });
        (probe, receiver)
    }
}

impl HostSessionProbe for GatedProbe {
    fn daemon_alive(&self, host_session: &HostSessionIdentity) -> bool {
        let gated = self
            .gated_socket
            .as_deref()
            .is_none_or(|socket| socket == host_session.socket);
        if !gated {
            return true;
        }
        if let Some(entered) = self
            .entered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = entered.send(());
        }
        self.gate.wait()
    }
    fn session_live(&self, _host_session: &HostSessionIdentity) -> bool {
        false
    }
    fn session_liveness(&self, _host_session: &HostSessionIdentity) -> HostSessionLiveness {
        HostSessionLiveness::Gone
    }
    fn refresh(&self, _socket: Option<&str>) {}
}

fn host_session(task_id: &str, socket: &str) -> HostSessionIdentity {
    HostSessionIdentity {
        socket: socket.to_string(),
        routing_id: format!("routing-{task_id}"),
        session_path: format!("/tmp/dh-fake/sessions/{task_id}.jsonl"),
        instance_id: "instance-1".to_string(),
        daemon_pid: None,
        effect_seq: None,
        adopted_entry_id: None,
    }
}

fn seed_host_record(
    store: &TaskRecordStore,
    task_id: &str,
    socket: &str,
    residency: ResidencyState,
    host_pid: Option<i64>,
) -> TaskRecord {
    seed_record(
        store,
        Seed {
            task_id,
            status: Some(TaskStatus::Running),
            residency_state: Some(residency),
            execution_mode: Some("process"),
            host_pid,
            runner_kind: Some(RunnerKind::HostSession),
            host_session: Some(host_session(task_id, socket)),
            ..Seed::default()
        },
    )
}

type Respawned = Arc<Mutex<Vec<String>>>;

fn deps(
    store: &Arc<TaskRecordStore>,
    probe: Arc<dyn HostSessionProbe>,
    respawned: &Respawned,
    respawn: Arc<dyn Fn(&TaskRecord) -> RespawnResult + Send + Sync>,
) -> LifecycleDeps {
    let mut deps = LifecycleDeps::new(
        store.clone(),
        Arc::new(FakeRegistry::default()),
        default_settings(),
    );
    deps.host_pid = Some(HOST_PID);
    deps.orphan_kill_delay_ms = Some(0);
    deps.signaller = Some(FakeSignaller::alive([]));
    deps.host_session_probe = Some(probe);
    // The 5 s live-parent scan is unfenced; it must not race the gated passes below.
    deps.live_parent_recovery =
        Some(crate::lifecycle::live_parent_recovery::RecoverySchedule::Disabled);
    let log = Arc::clone(respawned);
    deps.respawn = Some(Arc::new(move |record: &TaskRecord, _| {
        log.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(record.task_id.clone());
        respawn(record)
    }));
    deps.reattach = Some(Arc::new(|_, _| ReattachResult::Ok));
    deps
}

fn no_respawn() -> Arc<dyn Fn(&TaskRecord) -> RespawnResult + Send + Sync> {
    Arc::new(|_: &TaskRecord| RespawnResult::Failed {
        disposition: RespawnDisposition::Retryable,
        code: RespawnFailureCode::RespawnFailed,
        reason: "no respawn port".to_string(),
    })
}

fn start(
    lifecycle: &crate::lifecycle::TaskLifecycle,
) -> JoinHandle<Result<ReconcileResult, LifecycleError>> {
    let pass = lifecycle.clone();
    std::thread::spawn(move || pass.reconcile_on_session_start(Some("parent-1")))
}

fn respawned_ids(respawned: &Respawned) -> Vec<String> {
    respawned
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn load(store: &TaskRecordStore, task_id: &str) -> TaskRecord {
    store.load(task_id).expect("load").expect("record")
}

fn host_reach_completion_cannot_spawn(new_owner: bool) {
    let temp = temp_store();
    let record = seed_host_record(
        &temp.store,
        "st_10056004",
        "/tmp/dh-fake/rpc.sock",
        ResidencyState::RpcDetached,
        None,
    );
    let (probe, entered) = GatedProbe::new(None);
    let respawned = Respawned::default();
    let lifecycle =
        create_task_lifecycle(deps(&temp.store, probe.clone(), &respawned, no_respawn()));
    let pass = start(&lifecycle);
    entered
        .recv_timeout(SIGNAL_DEADLINE)
        .expect("host reach entered");
    if new_owner {
        temp.store
            .mutate(&record.task_id, |fresh| TaskRecord {
                residency_claim: Some("new-owner".to_string()),
                ..fresh.clone()
            })
            .expect("install newer claim");
    } else {
        lifecycle.dispose();
    }
    let before = load(&temp.store, &record.task_id);
    probe.gate.open(true);
    pass.join().expect("pass thread").expect("reconcile");

    assert!(respawned_ids(&respawned).is_empty());
    if new_owner {
        assert_eq!(load(&temp.store, &record.task_id), before);
    } else {
        assert_eq!(
            load(&temp.store, &record.task_id).residency_state,
            ResidencyState::RpcDetached
        );
    }
}

#[test]
fn host_reach_completion_after_dispose_cannot_spawn() {
    host_reach_completion_cannot_spawn(false);
}

#[test]
fn host_reach_completion_after_new_owner_cannot_spawn() {
    host_reach_completion_cannot_spawn(true);
}

/// A host-session handle whose turn never settles; it logs the teardown steps it receives.
struct LoggedHostHandle {
    task_id: String,
    calls: CallLog,
}

impl LoggedHostHandle {
    fn log(&self, step: &str) {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(step.to_string());
    }
}

impl ManagedChildHandle for LoggedHostHandle {
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
        self.log("abort");
        Ok(())
    }
    fn subscribe(&self, _listener: ManagedChildListener) -> Unsubscribe {
        Box::new(|| ())
    }
    fn wait_for_outcome(&self) -> RunnerOutcome {
        panic!("a late revival never waits on the suspended handle's turn");
    }
    fn last_assistant_text(&self) -> Option<String> {
        None
    }
    fn dispose(&self) -> Result<(), HostError> {
        self.log("dispose");
        Ok(())
    }
    fn is_host_session(&self) -> bool {
        true
    }
}

#[test]
fn a_respawn_returning_after_dispose_is_suspended_without_reattachment() {
    let temp = temp_store();
    let record = seed_host_record(
        &temp.store,
        "st_10056005",
        "/tmp/dh-fake/rpc.sock",
        ResidencyState::RpcDetached,
        None,
    );
    let (probe, _unused) = GatedProbe::new(Some("/never-gated.sock"));
    let steps = call_log();
    let gate = Arc::new(Gate::default());
    let (entered_sender, entered) = channel();
    let entered_sender = Mutex::new(Some(entered_sender));
    let respawn_gate = Arc::clone(&gate);
    let handle_calls = Arc::clone(&steps);
    let respawned = Respawned::default();
    let mut lifecycle_deps = deps(
        &temp.store,
        probe,
        &respawned,
        Arc::new(move |record: &TaskRecord| {
            if let Some(sender) = entered_sender
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                let _ = sender.send(());
            }
            respawn_gate.wait();
            RespawnResult::Ok(Arc::new(LoggedHostHandle {
                task_id: record.task_id.clone(),
                calls: Arc::clone(&handle_calls),
            }))
        }),
    );
    let reattach_calls = Arc::clone(&steps);
    lifecycle_deps.reattach = Some(Arc::new(move |_, _| {
        reattach_calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push("reattach".to_string());
        ReattachResult::Ok
    }));
    let lifecycle = create_task_lifecycle(lifecycle_deps);
    let pass = start(&lifecycle);
    entered
        .recv_timeout(SIGNAL_DEADLINE)
        .expect("respawn entered");
    lifecycle.dispose();
    gate.open(true);
    pass.join().expect("pass thread").expect("reconcile");

    assert_eq!(calls(&steps), vec!["dispose".to_string()]);
    assert_eq!(
        load(&temp.store, &record.task_id).residency_state,
        ResidencyState::RpcDetached
    );
}

fn initial_probe_after_stop_cannot_claim(shutdown: bool) {
    let temp = temp_store();
    let record = seed_host_record(
        &temp.store,
        "st_10056001",
        "/tmp/dh-fake/rpc.sock",
        ResidencyState::Resident,
        Some(DEAD_OWNER),
    );
    let (probe, entered) = GatedProbe::new(None);
    let respawned = Respawned::default();
    let lifecycle =
        create_task_lifecycle(deps(&temp.store, probe.clone(), &respawned, no_respawn()));
    let pass = start(&lifecycle);
    entered
        .recv_timeout(SIGNAL_DEADLINE)
        .expect("ownership probe entered");
    if shutdown {
        lifecycle
            .suspend_on_session_shutdown(&SuspendInput {
                parent_session_id: "parent-1".to_string(),
                own_task_id: None,
                reason: "quit".to_string(),
            })
            .expect("shutdown");
    } else {
        lifecycle.dispose();
    }
    probe.gate.open(false);
    pass.join().expect("pass thread").expect("reconcile");

    assert!(respawned_ids(&respawned).is_empty());
    assert_eq!(load(&temp.store, &record.task_id), record);
}

#[test]
fn initial_ownership_probe_completing_after_dispose_cannot_claim_or_revive() {
    initial_probe_after_stop_cannot_claim(false);
}

#[test]
fn initial_ownership_probe_completing_after_shutdown_cannot_claim_or_revive() {
    initial_probe_after_stop_cannot_claim(true);
}

#[test]
fn a_hung_host_does_not_delay_reconciliation_on_another_host() {
    let temp = temp_store();
    seed_host_record(
        &temp.store,
        "st_10056001",
        "/hung.sock",
        ResidencyState::Resident,
        Some(DEAD_OWNER),
    );
    seed_host_record(
        &temp.store,
        "st_10056002",
        "/ready.sock",
        ResidencyState::Resident,
        Some(DEAD_OWNER),
    );
    let (probe, _entered) = GatedProbe::new(Some("/hung.sock"));
    let (progressed_sender, progressed) = channel();
    let progressed_sender = Mutex::new(progressed_sender);
    let respawned = Respawned::default();
    let lifecycle = create_task_lifecycle(deps(
        &temp.store,
        probe.clone(),
        &respawned,
        Arc::new(move |_: &TaskRecord| {
            let _ = progressed_sender
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .send(());
            RespawnResult::Failed {
                disposition: RespawnDisposition::Retryable,
                code: RespawnFailureCode::ModelUnavailable,
                reason: "test".to_string(),
            }
        }),
    ));
    let pass = start(&lifecycle);
    let progressed = progressed.recv_timeout(SIGNAL_DEADLINE);
    let observed = respawned_ids(&respawned);
    lifecycle.dispose();
    probe.gate.open(false);
    pass.join().expect("pass thread").expect("reconcile");

    progressed.expect("the ready host progressed while the hung host was held");
    assert_eq!(observed, vec!["st_10056002".to_string()]);
}

#[test]
fn a_late_probe_cannot_reclaim_a_newer_same_process_ownership_claim() {
    let temp = temp_store();
    let original = seed_host_record(
        &temp.store,
        "st_10056003",
        "/tmp/dh-fake/rpc.sock",
        ResidencyState::Resident,
        Some(HOST_PID),
    );
    let (probe, entered) = GatedProbe::new(None);
    let respawned = Respawned::default();
    let lifecycle =
        create_task_lifecycle(deps(&temp.store, probe.clone(), &respawned, no_respawn()));
    let pass = start(&lifecycle);
    entered
        .recv_timeout(SIGNAL_DEADLINE)
        .expect("ownership probe entered");
    temp.store
        .replace(&TaskRecord {
            residency_claim: Some("new-owner".to_string()),
            ..original.clone()
        })
        .expect("install newer claim");
    let newer = load(&temp.store, &original.task_id);
    probe.gate.open(false);
    let result = pass.join().expect("pass thread").expect("reconcile");

    assert_eq!(load(&temp.store, &original.task_id), newer);
    assert!(respawned_ids(&respawned).is_empty());
    assert!(result.outcomes.iter().any(|outcome| {
        outcome.task_id == original.task_id
            && outcome.kind == ReconcileOutcomeKind::ForeignLiveOwner
            && outcome.reason.as_deref() == Some("ownership changed during probe")
    }));
}
