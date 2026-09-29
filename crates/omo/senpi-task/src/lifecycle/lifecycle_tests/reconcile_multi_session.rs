//! `lifecycle/reconcile-multi-session.test.ts`.

use std::sync::Arc;

use super::*;
use crate::lifecycle::{LifecycleDeps, ReconcileOutcomeKind, create_task_lifecycle};
use crate::state::{ResidencyState::*, TaskStatus::*};

const THIS_PID: i64 = 1111;
const FOREIGN_PID: i64 = 4242;

fn deps(temp: &TempStore, alive: &[i64]) -> LifecycleDeps {
    let mut deps = LifecycleDeps::new(
        temp.store.clone(),
        Arc::new(FakeRegistry::default()),
        default_settings(),
    );
    deps.now = Some(Arc::new(|| 5_000_000));
    deps.signaller = Some(Arc::new(FakeSignaller {
        dies_on_term: true,
        ..FakeSignaller::with_alive(alive.iter().copied())
    }));
    deps.orphan_kill_delay_ms = Some(0);
    deps.host_pid = Some(THIS_PID);
    deps
}

fn seed_other_session(temp: &TempStore, task_id: &str, host_pid: i64) {
    seed_record(
        &temp.store,
        Seed {
            task_id,
            parent_session_id: Some("session-a"),
            status: Some(Running),
            residency_state: Some(Resident),
            execution_mode: Some("in-process"),
            host_pid: Some(host_pid),
            ..Seed::default()
        },
    );
}

#[test]
fn same_process_sibling_resident_is_deferred_never_lost() {
    let temp = temp_store();
    seed_other_session(&temp, "st_00000030", THIS_PID);
    let result = create_task_lifecycle(deps(&temp, &[THIS_PID]))
        .reconcile_on_session_start(Some("session-b"))
        .expect("reconcile");
    let outcome = result
        .outcomes
        .iter()
        .find(|outcome| outcome.task_id == "st_00000030")
        .expect("outcome");
    assert_eq!(outcome.kind, ReconcileOutcomeKind::Deferred);
    assert_eq!(outcome.reason.as_deref(), Some("foreign_live_owner"));
    let record = temp
        .store
        .load("st_00000030")
        .expect("load")
        .expect("record");
    assert_eq!(record.status, Running);
    assert_eq!(record.residency_state, Resident);
    assert!(!read_events(&temp.store, "st_00000030").contains(&"reconcile_lost".to_string()));
}

#[test]
fn dead_foreign_owner_sibling_is_still_lost() {
    let temp = temp_store();
    seed_other_session(&temp, "st_00000031", FOREIGN_PID);
    let result = create_task_lifecycle(deps(&temp, &[]))
        .reconcile_on_session_start(Some("session-b"))
        .expect("reconcile");
    let outcome = result
        .outcomes
        .iter()
        .find(|outcome| outcome.task_id == "st_00000031")
        .expect("outcome");
    assert_eq!(outcome.kind, ReconcileOutcomeKind::Lost);
    assert_eq!(
        temp.store
            .load("st_00000031")
            .expect("load")
            .expect("record")
            .status,
        Lost
    );
}
