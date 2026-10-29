//! `lifecycle/ttl.test.ts`.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::lifecycle::{CleanupResult, LifecycleDeps, create_task_lifecycle};
use crate::state::{ResidencyState::*, TaskStatus::*};

const NOW: i64 = 100_000_000;
const TTL: i64 = 10_000;

fn age(age_ms: i64) -> Option<String> {
    Some(crate::shared::iso_from_ms(NOW - age_ms))
}

fn deps(store: Arc<dyn LifecycleStore>, registry: Arc<FakeRegistry>) -> LifecycleDeps {
    let mut deps = LifecycleDeps::new(store, registry, settings(json!({ "ttl_ms": TTL })));
    deps.now = Some(Arc::new(|| NOW));
    deps
}

fn cleanup(deps: LifecycleDeps) -> CleanupResult {
    create_task_lifecycle(deps)
        .cleanup_expired_records()
        .expect("cleanup")
}

fn has(ids: &[String], id: &str) -> bool {
    ids.iter().any(|candidate| candidate == id)
}

struct Artifacts {
    child_dir: PathBuf,
    spill_path: PathBuf,
    log_path: PathBuf,
}

fn seed_child_artifacts(store: &TaskRecordStore, task_id: &str) -> Artifacts {
    append_seed_event(store, task_id);
    let child_dir = store.state_dir().join("children").join(task_id);
    let session_dir = child_dir.join("sessions").join(task_id);
    std::fs::create_dir_all(&session_dir).expect("session dir");
    std::fs::write(session_dir.join("x.jsonl"), "{\"role\":\"user\"}\n").expect("transcript");
    let spill_dir = store.state_dir().join("completion-results");
    std::fs::create_dir_all(&spill_dir).expect("spill dir");
    let spill_path = spill_dir.join(format!("{task_id}.txt"));
    std::fs::write(&spill_path, "spilled final response").expect("spill");
    Artifacts {
        child_dir,
        spill_path,
        log_path: store
            .state_dir()
            .join("logs")
            .join(format!("{task_id}.jsonl")),
    }
}

fn simple(temp: &TempStore, seed: Seed<'_>) {
    seed_record(&temp.store, seed);
}

#[test]
fn expired_terminal_record_and_log_are_deleted() {
    let temp = temp_store();
    simple(
        &temp,
        Seed {
            task_id: "st_00000001",
            status: Some(Completed),
            updated_at: age(TTL + 1),
            ..Seed::default()
        },
    );
    append_seed_event(&temp.store, "st_00000001");
    let result = cleanup(deps(temp.store.clone(), Arc::default()));
    assert!(has(&result.deleted, "st_00000001"));
    assert!(!record_path(&temp.store, "st_00000001").exists());
    assert!(
        !temp
            .store
            .state_dir()
            .join("logs/st_00000001.jsonl")
            .exists()
    );
}

#[test]
fn fresh_terminal_record_is_retained() {
    let temp = temp_store();
    simple(
        &temp,
        Seed {
            task_id: "st_00000002",
            status: Some(Completed),
            updated_at: age(TTL - 1),
            ..Seed::default()
        },
    );
    let result = cleanup(deps(temp.store.clone(), Arc::default()));
    assert!(has(&result.retained, "st_00000002"));
    assert!(record_path(&temp.store, "st_00000002").exists());
}

#[test]
fn old_non_terminal_record_is_retained() {
    let temp = temp_store();
    simple(
        &temp,
        Seed {
            task_id: "st_00000003",
            status: Some(Running),
            updated_at: age(TTL + 1000),
            ..Seed::default()
        },
    );
    let result = cleanup(deps(temp.store.clone(), Arc::default()));
    assert!(has(&result.retained, "st_00000003"));
}

#[test]
fn freshly_claimed_pending_record_survives_far_future_cutoff() {
    let temp = temp_store();
    simple(
        &temp,
        Seed {
            task_id: "st_00000007",
            status: Some(Pending),
            updated_at: age(0),
            ..Seed::default()
        },
    );
    let mut deps = deps(temp.store.clone(), Arc::default());
    deps.now = Some(Arc::new(|| NOW + TTL * 100));
    let result = cleanup(deps);
    assert!(has(&result.retained, "st_00000007"));
    assert!(record_path(&temp.store, "st_00000007").exists());
}

fn lost_rpc(temp: &TempStore, task_id: &str, pid: i64) {
    simple(
        temp,
        Seed {
            task_id,
            status: Some(Lost),
            execution_mode: Some("process"),
            pid: Some(pid),
            updated_at: age(TTL + 1000),
            ..Seed::default()
        },
    );
}

#[test]
fn lost_rpc_record_with_live_pid_is_retained() {
    let temp = temp_store();
    lost_rpc(&temp, "st_00000004", 700);
    let mut deps = deps(temp.store.clone(), Arc::default());
    deps.signaller = Some(FakeSignaller::alive([700]));
    let result = cleanup(deps);
    assert!(has(&result.retained, "st_00000004"));
    assert!(record_path(&temp.store, "st_00000004").exists());
}

#[test]
fn lost_rpc_record_with_dead_pid_is_deleted() {
    let temp = temp_store();
    lost_rpc(&temp, "st_00000005", 701);
    let mut deps = deps(temp.store.clone(), Arc::default());
    deps.signaller = Some(FakeSignaller::alive([]));
    let result = cleanup(deps);
    assert!(has(&result.deleted, "st_00000005"));
}

#[test]
fn live_resident_handle_protects_record_until_forgotten() {
    let temp = temp_store();
    simple(
        &temp,
        Seed {
            task_id: "st_00000006",
            status: Some(Completed),
            updated_at: age(TTL + 1),
            ..Seed::default()
        },
    );
    let registry = Arc::new(FakeRegistry::default());
    registry.add(fake_handle(
        "st_00000006",
        ResidentKind::InProcess,
        &call_log(),
        HandleOptions::default(),
    ));
    let lifecycle = create_task_lifecycle(deps(temp.store.clone(), registry.clone()));
    let retained = lifecycle.cleanup_expired_records().expect("cleanup");
    assert!(has(&retained.retained, "st_00000006"));
    assert!(record_path(&temp.store, "st_00000006").exists());
    registry.forget("st_00000006");
    let after = lifecycle.cleanup_expired_records().expect("cleanup");
    assert!(has(&after.deleted, "st_00000006"));
    assert!(!record_path(&temp.store, "st_00000006").exists());
}

fn foreign_resident(temp: &TempStore, task_id: &str) {
    simple(
        temp,
        Seed {
            task_id,
            status: Some(Completed),
            residency_state: Some(Resident),
            updated_at: age(TTL + 1000),
            host_pid: Some(4242),
            ..Seed::default()
        },
    );
}

#[test]
fn resident_owned_by_live_foreign_process_is_retained() {
    let temp = temp_store();
    foreign_resident(&temp, "st_00000010");
    let mut deps = deps(temp.store.clone(), Arc::default());
    deps.signaller = Some(FakeSignaller::alive([4242]));
    deps.host_pid = Some(1111);
    let result = cleanup(deps);
    assert!(has(&result.retained, "st_00000010"));
    assert!(record_path(&temp.store, "st_00000010").exists());
}

#[test]
fn resident_whose_foreign_owner_is_dead_is_deleted() {
    let temp = temp_store();
    foreign_resident(&temp, "st_00000011");
    let mut deps = deps(temp.store.clone(), Arc::default());
    deps.signaller = Some(FakeSignaller::alive([]));
    deps.host_pid = Some(1111);
    let result = cleanup(deps);
    assert!(has(&result.deleted, "st_00000011"));
}

fn persisted(temp: &TempStore, task_id: &str, status: TaskStatus, age_ms: i64) {
    simple(
        temp,
        Seed {
            task_id,
            status: Some(status),
            residency_state: Some(PersistedOnly),
            updated_at: age(age_ms),
            ..Seed::default()
        },
    );
}

#[test]
fn expired_terminal_persisted_only_record_deletes_every_artifact() {
    let temp = temp_store();
    persisted(&temp, "st_00000020", Completed, TTL + 1);
    let artifacts = seed_child_artifacts(&temp.store, "st_00000020");
    let result = cleanup(deps(temp.store.clone(), Arc::default()));
    assert!(has(&result.deleted, "st_00000020"));
    assert!(!record_path(&temp.store, "st_00000020").exists());
    assert!(!artifacts.child_dir.exists());
    assert!(!artifacts.spill_path.exists());
    assert!(!artifacts.log_path.exists());
}

#[test]
fn expired_non_terminal_suspended_record_keeps_artifacts() {
    let temp = temp_store();
    persisted(&temp, "st_00000021", Running, TTL + 1000);
    let artifacts = seed_child_artifacts(&temp.store, "st_00000021");
    let result = cleanup(deps(temp.store.clone(), Arc::default()));
    assert!(has(&result.retained, "st_00000021"));
    assert!(record_path(&temp.store, "st_00000021").exists());
    assert!(artifacts.child_dir.exists());
    assert!(artifacts.spill_path.exists());
}

#[test]
fn revival_claim_between_scan_and_delete_is_retained() {
    const HOST: i64 = 1111;
    let temp = temp_store();
    persisted(&temp, "st_00000022", Completed, TTL + 1000);
    let store = RacingStore::new(
        &temp.store,
        Box::new(|inner: &TaskRecordStore| {
            inner
                .mutate("st_00000022", |record| {
                    let mut next = record.clone();
                    next.residency_state = Resident;
                    next.host_pid = Some(HOST);
                    next
                })
                .expect("racing claim");
        }),
    );
    let mut deps = deps(store, Arc::default());
    deps.signaller = Some(FakeSignaller::alive([HOST]));
    deps.host_pid = Some(HOST);
    let result = cleanup(deps);
    assert!(!has(&result.deleted, "st_00000022"));
    assert!(has(&result.retained, "st_00000022"));
    let record = temp
        .store
        .load("st_00000022")
        .expect("load")
        .expect("record");
    assert_eq!(record.residency_state, Resident);
    assert_eq!(record.host_pid, Some(HOST));
}

fn deliver_epoch(store: &TaskRecordStore, task_id: &str, epoch: i64) {
    store
        .mutate(task_id, |record| {
            let mut next = record.clone();
            next.notification.notified_epoch = epoch;
            next
        })
        .expect("deliver");
}

#[test]
fn undelivered_notify_on_terminal_protects_record_until_delivered() {
    let temp = temp_store();
    simple(
        &temp,
        Seed {
            task_id: "st_00000023",
            status: Some(Completed),
            residency_state: Some(PersistedOnly),
            updated_at: age(TTL + 1000),
            notify_on_terminal: true,
            run_epoch: Some(2),
            notified_epoch: Some(0),
            ..Seed::default()
        },
    );
    let lifecycle = create_task_lifecycle(deps(temp.store.clone(), Arc::default()));
    let first = lifecycle.cleanup_expired_records().expect("cleanup");
    assert!(has(&first.retained, "st_00000023"));
    assert!(record_path(&temp.store, "st_00000023").exists());
    deliver_epoch(&temp.store, "st_00000023", 2);
    let second = lifecycle.cleanup_expired_records().expect("cleanup");
    assert!(has(&second.deleted, "st_00000023"));
    assert!(!record_path(&temp.store, "st_00000023").exists());
}

#[test]
fn legacy_notification_failed_epoch_protects_record_until_delivered() {
    let temp = temp_store();
    simple(
        &temp,
        Seed {
            task_id: "st_00000024",
            status: Some(Completed),
            residency_state: Some(PersistedOnly),
            updated_at: age(TTL + 1000),
            run_epoch: Some(2),
            notified_epoch: Some(0),
            notification_failed_epoch: Some(2),
            ..Seed::default()
        },
    );
    let lifecycle = create_task_lifecycle(deps(temp.store.clone(), Arc::default()));
    let first = lifecycle.cleanup_expired_records().expect("cleanup");
    assert!(has(&first.retained, "st_00000024"));
    assert!(record_path(&temp.store, "st_00000024").exists());
    deliver_epoch(&temp.store, "st_00000024", 2);
    let second = lifecycle.cleanup_expired_records().expect("cleanup");
    assert!(has(&second.deleted, "st_00000024"));
    assert!(!record_path(&temp.store, "st_00000024").exists());
}

#[test]
fn crashed_sweep_tombstone_is_finished_idempotently() {
    let temp = temp_store();
    persisted(&temp, "st_00000025", Completed, TTL + 1000);
    let artifacts = seed_child_artifacts(&temp.store, "st_00000025");
    let record = record_path(&temp.store, "st_00000025");
    let tombstone = PathBuf::from(format!("{}.expunging", record.display()));
    std::fs::rename(&record, &tombstone).expect("tombstone");
    let lifecycle = create_task_lifecycle(deps(temp.store.clone(), Arc::default()));
    let first = lifecycle.cleanup_expired_records().expect("cleanup");
    assert!(has(&first.deleted, "st_00000025"));
    assert!(!tombstone.exists());
    assert!(!artifacts.child_dir.exists());
    assert!(!artifacts.spill_path.exists());
    assert!(!artifacts.log_path.exists());
    assert!(temp.store.load("st_00000025").expect("load").is_none());
    let second = lifecycle.cleanup_expired_records().expect("cleanup");
    assert!(!has(&second.deleted, "st_00000025"));
    assert!(!tombstone.exists());
    assert!(!artifacts.child_dir.exists());
}

#[test]
fn live_orphan_is_destroyed_before_artifacts_are_deleted() {
    let temp = temp_store();
    simple(
        &temp,
        Seed {
            task_id: "st_00000026",
            status: Some(Completed),
            residency_state: Some(RpcDetached),
            execution_mode: Some("process"),
            pid: Some(9001),
            updated_at: age(TTL + 1000),
            ..Seed::default()
        },
    );
    let artifacts = seed_child_artifacts(&temp.store, "st_00000026");
    let child_dir = artifacts.child_dir.clone();
    let existed = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&existed);
    let signaller = Arc::new(
        FakeSignaller::with_alive([9001]).with_hook(Box::new(move |_, _| {
            seen.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(child_dir.exists());
        })),
    );
    let mut deps = deps(temp.store.clone(), Arc::default());
    deps.signaller = Some(signaller.clone());
    deps.orphan_kill_delay_ms = Some(0);
    let result = cleanup(deps);
    assert_eq!(signaller.signals(), [(9001, "SIGTERM"), (9001, "SIGKILL")]);
    assert_eq!(
        *existed.lock().unwrap_or_else(PoisonError::into_inner),
        [true, true]
    );
    assert!(has(&result.deleted, "st_00000026"));
    assert!(!record_path(&temp.store, "st_00000026").exists());
    assert!(!artifacts.child_dir.exists());
}
