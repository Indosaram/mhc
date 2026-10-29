//! `lifecycle/reconcile.test.ts`: the cases that drive only `createTaskLifecycle`. The cases
//! built on `createHarness`/`createTaskManager` (respawn through a real manager) live with the
//! manager tests.

use std::sync::Arc;

use super::*;
use crate::lifecycle::port::{ResidencyRegistry, ResidentHandle, ResidentKind};
use crate::lifecycle::{
    LifecycleDeps, ReconcileOutcome, ReconcileOutcomeKind, create_task_lifecycle,
};
use crate::state::{ResidencyState::*, TaskStatus::*};

const FOREIGN_PID: i64 = 4242;
const THIS_PID: i64 = 1111;

fn deps(
    temp: &TempStore,
    registry: Arc<dyn ResidencyRegistry>,
    config: TaskSettings,
    signaller: Arc<FakeSignaller>,
    host_pid: Option<i64>,
) -> LifecycleDeps {
    let mut deps = LifecycleDeps::new(temp.store.clone(), registry, config);
    deps.now = Some(Arc::new(|| 5_000_000));
    deps.signaller = Some(signaller);
    deps.orphan_kill_delay_ms = Some(0);
    deps.host_pid = host_pid;
    deps
}

fn dying_signaller(alive: impl IntoIterator<Item = i64>) -> Arc<FakeSignaller> {
    Arc::new(FakeSignaller {
        dies_on_term: true,
        ..FakeSignaller::with_alive(alive)
    })
}

fn first_outcome(deps: LifecycleDeps) -> ReconcileOutcome {
    create_task_lifecycle(deps)
        .reconcile_on_session_start(None)
        .expect("reconcile")
        .outcomes
        .into_iter()
        .next()
        .expect("one outcome")
}

fn seed_resident(temp: &TempStore, task_id: &str, status: TaskStatus, host_pid: Option<i64>) {
    seed_record(
        &temp.store,
        Seed {
            task_id,
            status: Some(status),
            residency_state: Some(Resident),
            execution_mode: Some("in-process"),
            host_pid,
            ..Seed::default()
        },
    );
}

/// `get` always misses; `entries` still sees the live handle.
struct PointLookupMissRegistry(FakeRegistry);

impl ResidencyRegistry for PointLookupMissRegistry {
    fn get(&self, _task_id: &str) -> Option<Arc<dyn ResidentHandle>> {
        None
    }
    fn entries(&self) -> Vec<Arc<dyn ResidentHandle>> {
        self.0.entries()
    }
    fn forget(&self, task_id: &str) {
        self.0.forget(task_id);
    }
    fn has_pending_sends(&self, task_id: &str) -> bool {
        self.0.has_pending_sends(task_id)
    }
}

#[test]
fn live_in_process_resident_seen_only_in_the_snapshot_stays_resident() {
    let temp = temp_store();
    let order = call_log();
    let handle = fake_handle(
        "st_00000013",
        ResidentKind::InProcess,
        &order,
        HandleOptions::default(),
    );
    seed_resident(&temp, "st_00000013", Running, None);
    let inner = FakeRegistry::default();
    inner.add(handle.clone());
    let outcome = first_outcome(deps(
        &temp,
        Arc::new(PointLookupMissRegistry(inner)),
        default_settings(),
        dying_signaller([]),
        None,
    ));
    assert_eq!(
        outcome,
        ReconcileOutcome::new(
            "st_00000013",
            ReconcileOutcomeKind::Resumed,
            Some("owned by this process")
        )
    );
    let record = temp
        .store
        .load("st_00000013")
        .expect("load")
        .expect("record");
    assert_eq!(record.status, Running);
    assert_eq!(record.residency_state, Resident);
    assert!(!handle.did("dispose"));
}

#[test]
fn lost_in_process_record_releases_its_residency_claim() {
    let temp = temp_store();
    seed_resident(&temp, "st_00000011", Running, None);
    let outcome = first_outcome(deps(
        &temp,
        Arc::new(FakeRegistry::default()),
        default_settings(),
        dying_signaller([]),
        None,
    ));
    assert_eq!(outcome.kind, ReconcileOutcomeKind::Lost);
    let record = temp
        .store
        .load("st_00000011")
        .expect("load")
        .expect("record");
    assert_eq!(record.status, Lost);
    assert_eq!(record.residency_state, Disposed);
}

#[test]
fn leaked_lost_resident_record_self_heals_to_disposed() {
    let temp = temp_store();
    seed_resident(&temp, "st_00000012", Lost, None);
    let outcome = first_outcome(deps(
        &temp,
        Arc::new(FakeRegistry::default()),
        default_settings(),
        dying_signaller([]),
        None,
    ));
    assert_eq!(outcome.kind, ReconcileOutcomeKind::Lost);
    assert_eq!(residency(&temp.store, "st_00000012"), Some(Disposed));
}

#[test]
fn in_process_record_of_a_live_foreign_owner_is_skipped() {
    let temp = temp_store();
    seed_resident(&temp, "st_00000022", Running, Some(FOREIGN_PID));
    let outcome = first_outcome(deps(
        &temp,
        Arc::new(FakeRegistry::default()),
        default_settings(),
        dying_signaller([FOREIGN_PID]),
        Some(THIS_PID),
    ));
    assert_eq!(outcome.kind, ReconcileOutcomeKind::ForeignLiveOwner);
    let record = temp
        .store
        .load("st_00000022")
        .expect("load")
        .expect("record");
    assert_eq!(record.status, Running);
    assert_eq!(record.residency_state, Resident);
    assert!(!read_events(&temp.store, "st_00000022").contains(&"reconcile_lost".to_string()));
}

/// `crossProcessHarness`: a running process-mode resident (child pid 900 alive).
fn seed_cross_process(
    temp: &TempStore,
    host_pid: Option<i64>,
    owner_alive: bool,
) -> Arc<FakeSignaller> {
    seed_record(
        &temp.store,
        Seed {
            task_id: "st_00000021",
            status: Some(Running),
            residency_state: Some(Resident),
            execution_mode: Some("process"),
            pid: Some(900),
            host_pid,
            ..Seed::default()
        },
    );
    let mut alive = vec![900];
    if owner_alive && let Some(pid) = host_pid {
        alive.push(pid);
    }
    dying_signaller(alive)
}

fn cross_process_outcome(temp: &TempStore, signaller: &Arc<FakeSignaller>) -> ReconcileOutcome {
    first_outcome(deps(
        temp,
        Arc::new(FakeRegistry::default()),
        settings(serde_json::json!({ "reattach_on_reconcile": false })),
        Arc::clone(signaller),
        Some(THIS_PID),
    ))
}

#[test]
fn process_record_of_a_live_foreign_owner_is_skipped_without_signals() {
    let temp = temp_store();
    let signaller = seed_cross_process(&temp, Some(FOREIGN_PID), true);
    assert_eq!(
        cross_process_outcome(&temp, &signaller).kind,
        ReconcileOutcomeKind::ForeignLiveOwner
    );
    let record = temp
        .store
        .load("st_00000021")
        .expect("load")
        .expect("record");
    assert_eq!(record.status, Running);
    assert_eq!(record.residency_state, Resident);
    assert!(signaller.signals().is_empty());
    assert!(!read_events(&temp.store, "st_00000021").contains(&"reconcile_lost".to_string()));
}

#[test]
fn process_record_of_a_dead_owner_is_lost_and_terminated() {
    let temp = temp_store();
    let signaller = seed_cross_process(&temp, Some(FOREIGN_PID), false);
    assert_eq!(
        cross_process_outcome(&temp, &signaller).kind,
        ReconcileOutcomeKind::LostAndTerminated
    );
    assert_eq!(signaller.signals(), vec![(900, "SIGTERM")]);
    assert_eq!(
        temp.store
            .load("st_00000021")
            .expect("load")
            .expect("record")
            .status,
        Lost
    );
}

#[test]
fn legacy_process_record_without_host_pid_is_lost_and_terminated() {
    let temp = temp_store();
    let signaller = seed_cross_process(&temp, None, false);
    assert_eq!(
        cross_process_outcome(&temp, &signaller).kind,
        ReconcileOutcomeKind::LostAndTerminated
    );
    assert_eq!(signaller.signals(), vec![(900, "SIGTERM")]);
    assert_eq!(
        temp.store
            .load("st_00000021")
            .expect("load")
            .expect("record")
            .status,
        Lost
    );
}

#[test]
fn in_process_record_of_this_process_without_live_handle_is_lost() {
    let temp = temp_store();
    seed_resident(&temp, "st_00000022", Running, Some(THIS_PID));
    let outcome = first_outcome(deps(
        &temp,
        Arc::new(FakeRegistry::default()),
        default_settings(),
        dying_signaller([THIS_PID]),
        Some(THIS_PID),
    ));
    assert_eq!(outcome.kind, ReconcileOutcomeKind::Lost);
    assert_eq!(
        temp.store
            .load("st_00000022")
            .expect("load")
            .expect("record")
            .status,
        Lost
    );
}
