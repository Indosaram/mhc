//! `lifecycle/batch-admission.test.ts`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde_json::json;

use super::*;
use crate::lifecycle::admission_lease::{AcquireAdmissionLeaseResult, SessionAdmissionLease};
use crate::lifecycle::residency::{
    BatchAdmissionDeferral, BatchAdmissionOptions, BatchAdmissionOutcome, BatchLeaseState,
    ResidencyClaimResult, admit_suspended_batch, reclaim_orphaned_resident,
};
use crate::lifecycle::{LifecycleContext, LifecycleDeps, resolve_context};
use crate::state::{ResidencyState::*, TaskStatus::*};

fn host() -> i64 {
    i64::from(std::process::id())
}

fn context_for(store: &Arc<TaskRecordStore>, cap: Value) -> LifecycleContext {
    resolve_context(LifecycleDeps::new(
        store.clone(),
        Arc::new(FakeRegistry::default()),
        settings(json!({ "residency_max_children": cap })),
    ))
}

fn seed(
    temp: &TempStore,
    task_id: &str,
    status: TaskStatus,
    residency: ResidencyState,
    offset: Option<i64>,
    host_pid: Option<i64>,
) -> TaskRecord {
    seed_record(
        &temp.store,
        Seed {
            task_id,
            status: Some(status),
            residency_state: Some(residency),
            updated_at: offset.map(iso),
            host_pid,
            ..Seed::default()
        },
    )
}

fn state(store: &TaskRecordStore, task_id: &str) -> ResidencyState {
    residency(store, task_id).expect("record")
}

fn host_pid(store: &TaskRecordStore, task_id: &str) -> Option<i64> {
    store.load(task_id).expect("load").expect("record").host_pid
}

fn claimed(task_id: &str) -> BatchAdmissionOutcome {
    BatchAdmissionOutcome::Claimed {
        task_id: task_id.to_string(),
    }
}

fn deferred(task_id: &str, reason: BatchAdmissionDeferral) -> BatchAdmissionOutcome {
    BatchAdmissionOutcome::Deferred {
        task_id: task_id.to_string(),
        reason,
    }
}

fn is_claimed(outcome: &BatchAdmissionOutcome) -> bool {
    matches!(outcome, BatchAdmissionOutcome::Claimed { .. })
}

#[test]
fn unlimited_residency_claims_every_candidate() {
    let temp = temp_store();
    seed(&temp, "st_000000d0", Running, Resident, None, None);
    seed(&temp, "st_000000d1", Completed, Resident, None, None);
    seed(&temp, "st_000000d2", Running, PersistedOnly, None, None);
    seed(&temp, "st_000000d3", Completed, RpcDetached, None, None);
    seed(&temp, "st_000000d4", Error, PersistedOnly, None, None);
    let context = context_for(&temp.store, json!("unlimited"));
    let result = admit_suspended_batch(&context, "parent-1", &BatchAdmissionOptions::default())
        .expect("batch");
    assert_eq!(result.outcomes.len(), 3);
    assert!(result.outcomes.iter().all(is_claimed));
    for id in ["st_000000d2", "st_000000d3", "st_000000d4"] {
        assert_eq!(state(&temp.store, id), Resident);
    }
}

#[test]
fn cap_admits_running_first_then_mru_terminals() {
    let temp = temp_store();
    seed(&temp, "st_000000e8", Running, Resident, None, Some(host()));
    seed(
        &temp,
        "st_000000e9",
        Completed,
        Resident,
        None,
        Some(424_242),
    );
    seed(&temp, "st_00000010", Running, PersistedOnly, Some(5), None);
    seed(&temp, "st_00000011", Running, RpcDetached, Some(15), None);
    seed(&temp, "st_00000012", Pending, PersistedOnly, Some(25), None);
    seed(
        &temp,
        "st_00000020",
        Completed,
        PersistedOnly,
        Some(0),
        None,
    );
    seed(&temp, "st_00000021", Completed, RpcDetached, Some(10), None);
    seed(&temp, "st_00000022", Error, PersistedOnly, Some(20), None);
    seed(
        &temp,
        "st_00000023",
        Interrupted,
        PersistedOnly,
        Some(30),
        None,
    );
    seed(&temp, "st_00000024", Completed, RpcDetached, Some(40), None);
    seed(
        &temp,
        "st_00000025",
        Completed,
        PersistedOnly,
        Some(50),
        None,
    );
    seed(
        &temp,
        "st_00000026",
        Completed,
        PersistedOnly,
        Some(50),
        None,
    );
    let context = context_for(&temp.store, json!(8));
    let result = admit_suspended_batch(&context, "parent-1", &BatchAdmissionOptions::default())
        .expect("batch");
    assert_eq!(result.lease, BatchLeaseState::Acquired);
    let ids: Vec<&str> = result
        .outcomes
        .iter()
        .map(BatchAdmissionOutcome::task_id)
        .collect();
    assert_eq!(
        ids,
        [
            "st_00000012",
            "st_00000011",
            "st_00000010",
            "st_00000025",
            "st_00000026",
            "st_00000024",
            "st_00000023",
            "st_00000022",
            "st_00000021",
            "st_00000020",
        ]
    );
    assert!(result.outcomes[..6].iter().all(is_claimed));
    for outcome in &result.outcomes[6..] {
        assert_eq!(
            outcome,
            &deferred(outcome.task_id(), BatchAdmissionDeferral::Capacity)
        );
    }
    assert_eq!(state(&temp.store, "st_00000012"), Resident);
    assert_eq!(host_pid(&temp.store, "st_00000012"), Some(host()));
    assert_eq!(state(&temp.store, "st_00000025"), Resident);
    assert_eq!(state(&temp.store, "st_00000023"), PersistedOnly);
    assert_eq!(state(&temp.store, "st_00000020"), PersistedOnly);
    assert_eq!(state(&temp.store, "st_000000e8"), Resident);
    assert_eq!(state(&temp.store, "st_000000e9"), Resident);
    assert_eq!(host_pid(&temp.store, "st_000000e9"), Some(424_242));
}

#[test]
fn over_cap_revives_nothing_and_keeps_residents() {
    let temp = temp_store();
    seed(&temp, "st_000000e0", Running, Resident, None, Some(host()));
    seed(&temp, "st_000000e1", Running, Resident, None, Some(host()));
    seed(
        &temp,
        "st_000000e2",
        Completed,
        Resident,
        None,
        Some(424_242),
    );
    seed(&temp, "st_00000030", Running, PersistedOnly, None, None);
    seed(&temp, "st_00000031", Completed, RpcDetached, None, None);
    let context = context_for(&temp.store, json!(2));
    let result = admit_suspended_batch(&context, "parent-1", &BatchAdmissionOptions::default())
        .expect("batch");
    assert_eq!(result.lease, BatchLeaseState::Acquired);
    assert_eq!(result.outcomes.len(), 2);
    for outcome in &result.outcomes {
        assert_eq!(
            outcome,
            &deferred(outcome.task_id(), BatchAdmissionDeferral::Capacity)
        );
    }
    assert_eq!(state(&temp.store, "st_00000030"), PersistedOnly);
    assert_eq!(state(&temp.store, "st_00000031"), RpcDetached);
    for id in ["st_000000e0", "st_000000e1", "st_000000e2"] {
        assert_eq!(state(&temp.store, id), Resident);
    }
    assert_eq!(host_pid(&temp.store, "st_000000e2"), Some(424_242));
}

#[test]
fn only_this_sessions_revivable_records_are_claimed() {
    let temp = temp_store();
    seed_record(
        &temp.store,
        Seed {
            task_id: "st_00000040",
            parent_session_id: Some("other"),
            status: Some(Running),
            residency_state: Some(PersistedOnly),
            ..Seed::default()
        },
    );
    seed(&temp, "st_00000041", Cancelled, PersistedOnly, None, None);
    seed_record(
        &temp.store,
        Seed {
            task_id: "st_00000042",
            status: Some(Running),
            residency_state: Some(PersistedOnly),
            killed: true,
            ..Seed::default()
        },
    );
    seed(&temp, "st_00000043", Running, PersistedOnly, None, None);
    let context = context_for(&temp.store, json!(8));
    let result = admit_suspended_batch(&context, "parent-1", &BatchAdmissionOptions::default())
        .expect("batch");
    assert_eq!(result.outcomes, [claimed("st_00000043")]);
    for id in ["st_00000040", "st_00000041", "st_00000042"] {
        assert_eq!(state(&temp.store, id), PersistedOnly);
    }
}

#[test]
fn contended_lease_defers_the_whole_batch() {
    let temp = temp_store();
    seed(&temp, "st_00000050", Running, PersistedOnly, None, None);
    seed(&temp, "st_00000051", Completed, RpcDetached, None, None);
    let context = context_for(&temp.store, json!(8));
    let acquire_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&acquire_calls);
    let options = BatchAdmissionOptions {
        acquire_lease: Some(Arc::new(move |_: &Path, _: &str, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(AcquireAdmissionLeaseResult::Contended)
        })),
        ..BatchAdmissionOptions::default()
    };
    let result = admit_suspended_batch(&context, "parent-1", &options).expect("batch");
    assert_eq!(acquire_calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.lease, BatchLeaseState::LockContended);
    assert_eq!(result.outcomes.len(), 2);
    for outcome in &result.outcomes {
        assert_eq!(
            outcome,
            &deferred(outcome.task_id(), BatchAdmissionDeferral::LockContended)
        );
    }
    assert_eq!(state(&temp.store, "st_00000050"), PersistedOnly);
    assert_eq!(state(&temp.store, "st_00000051"), RpcDetached);
}

struct DisplacedLease {
    path: PathBuf,
    checks: AtomicUsize,
    released: Arc<AtomicBool>,
}

impl SessionAdmissionLease for DisplacedLease {
    fn token(&self) -> &str {
        "displaced"
    }
    fn path(&self) -> &Path {
        &self.path
    }
    fn is_owner(&self) -> bool {
        self.checks.fetch_add(1, Ordering::SeqCst) == 0
    }
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
    }
}

#[test]
fn displaced_lease_aborts_remaining_claims() {
    let temp = temp_store();
    seed(
        &temp,
        "st_00000060",
        Completed,
        PersistedOnly,
        Some(0),
        None,
    );
    seed(
        &temp,
        "st_00000061",
        Completed,
        PersistedOnly,
        Some(10),
        None,
    );
    seed(
        &temp,
        "st_00000062",
        Completed,
        PersistedOnly,
        Some(20),
        None,
    );
    let context = context_for(&temp.store, json!(8));
    let released = Arc::new(AtomicBool::new(false));
    let lease: Arc<dyn SessionAdmissionLease> = Arc::new(DisplacedLease {
        path: temp
            .store
            .state_dir()
            .join("locks")
            .join("session-parent-1.lock"),
        checks: AtomicUsize::new(0),
        released: Arc::clone(&released),
    });
    let options = BatchAdmissionOptions {
        acquire_lease: Some(Arc::new(move |_: &Path, _: &str, _| {
            Ok(AcquireAdmissionLeaseResult::Acquired(Arc::clone(&lease)))
        })),
        ..BatchAdmissionOptions::default()
    };
    let result = admit_suspended_batch(&context, "parent-1", &options).expect("batch");
    assert_eq!(result.lease, BatchLeaseState::LeaseLost);
    assert_eq!(
        result.outcomes,
        [
            claimed("st_00000062"),
            deferred("st_00000061", BatchAdmissionDeferral::LeaseLost),
            deferred("st_00000060", BatchAdmissionDeferral::LeaseLost),
        ]
    );
    assert_eq!(state(&temp.store, "st_00000062"), Resident);
    assert_eq!(state(&temp.store, "st_00000061"), PersistedOnly);
    assert_eq!(state(&temp.store, "st_00000060"), PersistedOnly);
    assert!(released.load(Ordering::SeqCst));
}

#[test]
fn reclaiming_an_orphan_bypasses_the_cap() {
    let temp = temp_store();
    seed(&temp, "st_00000070", Running, Resident, None, Some(host()));
    seed(&temp, "st_00000071", Running, Resident, None, Some(host()));
    let orphan = seed(
        &temp,
        "st_00000072",
        Completed,
        Resident,
        None,
        Some(999_999),
    );
    let context = context_for(&temp.store, json!(2));
    let result = reclaim_orphaned_resident(&context, &orphan).expect("reclaim");
    assert_eq!(result, ResidencyClaimResult::Claimed);
    assert_eq!(host_pid(&temp.store, "st_00000072"), Some(host()));
    assert_eq!(state(&temp.store, "st_00000072"), Resident);
    assert_eq!(state(&temp.store, "st_00000070"), Resident);
    assert_eq!(state(&temp.store, "st_00000071"), Resident);
}

#[test]
fn stale_observation_loses_the_reclaim_cas() {
    let temp = temp_store();
    let orphan = seed(
        &temp,
        "st_00000080",
        Completed,
        Resident,
        None,
        Some(999_999),
    );
    let context = context_for(&temp.store, json!(2));
    assert_eq!(
        reclaim_orphaned_resident(&context, &orphan).expect("reclaim"),
        ResidencyClaimResult::Claimed
    );
    let loser = reclaim_orphaned_resident(&context, &orphan).expect("reclaim");
    assert_eq!(loser, ResidencyClaimResult::NotClaimable);
    assert_eq!(host_pid(&temp.store, "st_00000080"), Some(host()));
}
