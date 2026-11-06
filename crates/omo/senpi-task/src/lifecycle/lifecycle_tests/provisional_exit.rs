//! `lifecycle/provisional-exit.ts` through reconciliation: the lifecycle-side cases of
//! `manager/provisional-exit.test.ts`, `manager/provisional-exit-retry.test.ts` and
//! `lifecycle/own-crash-reconcile.test.ts`. The actual manager writer of `provisional_exit`
//! and 3 deterministic tests exist in `manager/provisional_exit.rs` and
//! `manager/manager_tests/provisional_exit.rs`. The tests here exercise the lifecycle
//! reconciliation and recovery paths with seeded exits.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::time::Duration;

use super::deferred_owner_reconcile::WaitGates;
use super::*;
use crate::lifecycle::port::{
    ReattachResult, RespawnDisposition, RespawnFailureCode, RespawnResult,
};
use crate::lifecycle::provisional_exit::reconcile_provisional_exit;
use crate::lifecycle::{
    LifecycleDeps, NoHostEndpoint, ReconcileOutcomeKind, TaskLifecycle, create_task_lifecycle,
};
use crate::state::{ProvisionalExit, ResidencyState, TaskStatus, TaskTransition};

const DEADLINE: Duration = Duration::from_secs(10);
const TASK_ID: &str = "st_97160001";
const DEAD_OWNER: i64 = 7401;
const HOST_PID: i64 = 7501;

fn seed_exited(temp: &TempStore, parent: &str) -> TaskRecord {
    seed_record(
        &temp.store,
        Seed {
            task_id: TASK_ID,
            parent_session_id: Some(parent),
            status: Some(TaskStatus::Running),
            execution_mode: Some("process"),
            host_pid: Some(DEAD_OWNER),
            pid: Some(900),
            ..Seed::default()
        },
    );
    let dir = crate::lifecycle::reconcile::child_session_dir(temp.store.state_dir(), TASK_ID);
    std::fs::create_dir_all(&dir).expect("session dir");
    std::fs::write(dir.join("transcript.jsonl"), "{}\n").expect("transcript");
    temp.store
        .mutate(TASK_ID, |record| {
            let mut next = record.clone();
            next.provisional_exit = Some(ProvisionalExit {
                observed_at: crate::shared::iso_from_ms(now_ms()),
                run_epoch: record.notification.run_epoch,
                code: Some(1),
                signal: None,
            });
            next
        })
        .expect("provisional exit")
        .expect("record")
}

struct Harness {
    lifecycle: TaskLifecycle,
    respawns: Arc<AtomicUsize>,
    signaller: Arc<FakeSignaller>,
}

fn harness(
    temp: &TempStore,
    alive: &[i64],
    wait: Option<crate::lifecycle::context::RetryWaitFn>,
) -> Harness {
    let respawns = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&respawns);
    let signaller = FakeSignaller::alive(alive.iter().copied());
    let mut deps = LifecycleDeps::new(
        temp.store.clone(),
        Arc::new(FakeRegistry::default()),
        settings(json!({ "reattach_on_reconcile": true })),
    );
    deps.host_pid = Some(HOST_PID);
    deps.host_endpoint = Some(Arc::new(NoHostEndpoint));
    deps.signaller = Some(signaller.clone());
    deps.orphan_kill_delay_ms = Some(0);
    // The 5 s live-parent scan is unfenced; it must not race the gated passes below.
    deps.live_parent_recovery =
        Some(crate::lifecycle::live_parent_recovery::RecoverySchedule::Disabled);
    deps.respawn = Some(Arc::new(move |_, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        RespawnResult::Failed {
            disposition: RespawnDisposition::Unrecoverable,
            code: RespawnFailureCode::RespawnFailed,
            reason: "provisional exits are never respawned".to_string(),
        }
    }));
    deps.reattach = Some(Arc::new(|_, _| ReattachResult::Ok));
    deps.host_retry = Some(crate::lifecycle::HostSessionRetryPolicy {
        daemon_loss_backoff_ms: Vec::new(),
        deferred_retry_backoff_ms: vec![1],
        ..crate::lifecycle::default_host_session_retry_policy()
    });
    deps.host_retry_wait = wait;
    Harness {
        lifecycle: create_task_lifecycle(deps),
        respawns,
        signaller,
    }
}

fn load(temp: &TempStore) -> TaskRecord {
    temp.store.load(TASK_ID).expect("load").expect("record")
}

fn assert_lost_without_replay(temp: &TempStore, h: &Harness) {
    let record = load(temp);
    assert_eq!(record.status, TaskStatus::Lost);
    assert_eq!(record.provisional_exit, None);
    assert!(read_events(&temp.store, TASK_ID).contains(&"reconcile_lost".to_string()));
    assert_eq!(h.respawns.load(Ordering::SeqCst), 0);
}

fn owner_death_ends_lost(session: Option<&str>) {
    // Given an unconfirmed own-crash whose owning manager died inside the confirmation window.
    let temp = temp_store();
    seed_exited(&temp, "parent-1");
    let h = harness(&temp, &[], None);
    // When either reconciliation scope sees the owner gone.
    let outcomes = h
        .lifecycle
        .reconcile_on_session_start(session)
        .expect("reconcile")
        .outcomes;
    // Then the conservative contract is LOST, never a replay of the crashed child's transcript.
    assert!(outcomes.iter().any(|outcome| {
        outcome.task_id == TASK_ID && outcome.kind == ReconcileOutcomeKind::Lost
    }));
    assert_lost_without_replay(&temp, &h);
}

#[test]
fn own_crash_then_parent_death_ends_lost_same_session() {
    owner_death_ends_lost(Some("parent-1"));
}

#[test]
fn own_crash_then_parent_death_ends_lost_new_session() {
    owner_death_ends_lost(Some("new-parent"));
}

#[test]
fn own_crash_then_parent_death_ends_lost_unscoped() {
    owner_death_ends_lost(None);
}

fn shutdown_detached_exit_is_lost(session: Option<&str>) {
    // Given shutdown detached the dead child's record before the process exited.
    let temp = temp_store();
    seed_exited(&temp, "parent-1");
    temp.store
        .transition(
            TASK_ID,
            &TaskTransition::DetachRpc {
                timestamp: crate::shared::iso_from_ms(now_ms()),
            },
        )
        .expect("detach");
    assert_eq!(load(&temp).residency_state, ResidencyState::RpcDetached);
    assert!(load(&temp).provisional_exit.is_some());
    let h = harness(&temp, &[], None);
    // When the parent resumes, suspension must not erase the unconfirmed exit.
    h.lifecycle
        .reconcile_on_session_start(session)
        .expect("reconcile");
    assert_lost_without_replay(&temp, &h);
}

#[test]
fn a_shutdown_detached_provisional_exit_is_lost_same_session() {
    shutdown_detached_exit_is_lost(Some("parent-1"));
}

#[test]
fn a_shutdown_detached_provisional_exit_is_lost_new_session() {
    shutdown_detached_exit_is_lost(Some("new-parent"));
}

#[test]
fn a_shutdown_detached_provisional_exit_is_lost_unscoped() {
    shutdown_detached_exit_is_lost(None);
}

#[test]
fn a_committed_own_crash_error_survives_a_later_parent_death() {
    // Given the surviving manager committed ERROR, which drops the provisional exit.
    let temp = temp_store();
    seed_exited(&temp, "parent-1");
    temp.store
        .transition(
            TASK_ID,
            &TaskTransition::Fail {
                timestamp: crate::shared::iso_from_ms(now_ms()),
                error_message: "child exited with code 1".to_string(),
                killed: false,
                run_stats: None,
            },
        )
        .expect("fail");
    let failed = load(&temp);
    assert_eq!(failed.provisional_exit, None);
    let h = harness(&temp, &[], None);
    // When parent death occurs after ERROR was committed, reconciliation cannot relabel it.
    h.lifecycle
        .reconcile_on_session_start(Some("new-parent"))
        .expect("reconcile");
    let reconciled = load(&temp);
    assert_eq!(reconciled.status, TaskStatus::Error);
    assert_eq!(reconciled.error_message, failed.error_message);
    assert_eq!(
        reconciled.notification.run_epoch,
        failed.notification.run_epoch
    );
    assert!(!read_events(&temp.store, TASK_ID).contains(&"reconcile_lost".to_string()));
}

#[test]
fn a_stale_provisional_exit_observation_cannot_claim() {
    // Given a reconcile that observed an older provisional exit than the record now carries.
    let temp = temp_store();
    let observed = seed_exited(&temp, "parent-1");
    temp.store
        .mutate(TASK_ID, |record| {
            let mut next = record.clone();
            if let Some(exit) = next.provisional_exit.as_mut() {
                exit.observed_at = "2030-01-01T00:00:00.000Z".to_string();
            }
            next
        })
        .expect("newer exit");
    let current = load(&temp);
    let stale = TaskRecord {
        updated_at: current.updated_at.clone(),
        ..observed
    };
    let h = harness(&temp, &[], None);
    // When the stale observation tries to claim the record.
    let outcome =
        reconcile_provisional_exit(h.lifecycle.context(), &stale).expect("reconcile provisional");
    // Then the fence refuses and the record is untouched.
    assert_eq!(outcome.kind, ReconcileOutcomeKind::ForeignLiveOwner);
    assert_eq!(
        outcome.reason.as_deref(),
        Some("provisional exit claim changed")
    );
    assert_eq!(load(&temp), current);
}

#[test]
fn a_deferred_owner_retry_loses_a_provisional_exit_instead_of_reviving_it() {
    // Given another session's provisional exit whose dying owner still appears alive at startup.
    let temp = temp_store();
    seed_exited(&temp, "old-parent");
    let gates = Arc::new(WaitGates::default());
    let (entered_tx, entered) = channel();
    let h = harness(&temp, &[DEAD_OWNER], Some(gates.wait_fn(entered_tx)));
    let initial = h
        .lifecycle
        .reconcile_on_session_start(Some("new-parent"))
        .expect("reconcile")
        .outcomes;
    assert_eq!(initial[0].kind, ReconcileOutcomeKind::Deferred);
    assert_eq!(entered.recv_timeout(DEADLINE), Ok(0));
    // When only the background retry sees the owner finally disappear.
    h.signaller.set_alive(DEAD_OWNER, false);
    gates.release(0);
    assert!(
        h.lifecycle
            .context()
            .deferred_owner_retries
            .wait_settled(TASK_ID, DEADLINE),
        "the retry settled the provisional exit"
    );
    // Then it took the provisional-to-LOST path, never resuming the transcript.
    assert_lost_without_replay(&temp, &h);
    h.lifecycle.dispose();
    gates.release_all();
}

#[test]
fn same_session_detached_retry_loses_a_provisional_exit_without_replay() {
    let temp = temp_store();
    seed_exited(&temp, "parent-1");
    temp.store
        .transition(
            TASK_ID,
            &TaskTransition::DetachRpc {
                timestamp: crate::shared::iso_from_ms(now_ms()),
            },
        )
        .expect("detach");
    let h = harness(&temp, &[], Some(Arc::new(|_| {})));
    crate::lifecycle::deferred_revival::retry_deferred_scoped_child(
        h.lifecycle.context(),
        TASK_ID,
        "parent-1",
        "lock_contended",
    )
    .expect("scoped retry");
    assert_lost_without_replay(&temp, &h);
    h.lifecycle.dispose();
}

#[test]
fn stopped_scoped_retry_cannot_lose_or_revive_a_child() {
    let temp = temp_store();
    let original = seed_exited(&temp, "parent-1");
    let h = harness(&temp, &[], Some(Arc::new(|_| {})));
    crate::lifecycle::deferred_revival::stop_scoped_retries(h.lifecycle.context(), "parent-1");
    crate::lifecycle::deferred_revival::retry_deferred_scoped_child(
        h.lifecycle.context(),
        TASK_ID,
        "parent-1",
        "lock_contended",
    )
    .expect("stopped retry");
    assert_eq!(load(&temp), original);
    assert_eq!(h.respawns.load(Ordering::SeqCst), 0);
    h.lifecycle.dispose();
}
