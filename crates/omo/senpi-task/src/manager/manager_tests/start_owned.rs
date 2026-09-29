//! `manager/start-owned.test.ts`.

use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use sha2::{Digest, Sha256};

use super::fakes::{
    FakeRunner, HarnessOptions, Project, base_spec, config, lock, make_manager, notify, wait_until,
};
use crate::manager::types::{
    ManagedRunner, ManagedRunners, ManagerStartSpec, OwnedStartResult, SpawnAdmission, StartResult,
    TaskManagerOptions,
};
use crate::manager::{TaskManager, create_task_manager};
use crate::shared::{DagOwnerKind, DagTaskOwner};
use crate::state::{TaskRecord, TaskStatus};
use crate::store::{StoreError, TaskRecordSaver, TaskRecordStore};

fn owner() -> DagTaskOwner {
    DagTaskOwner {
        kind: DagOwnerKind::Dag,
        run_id: "run-1".to_string(),
        node_id: "node-1".to_string(),
        fingerprint: "fingerprint-1".to_string(),
    }
}

fn started_fresh(result: &OwnedStartResult) -> (&str, bool) {
    match result {
        OwnedStartResult::Started { task, reused } => (task.task_id.as_str(), *reused),
        other => panic!("expected started, got {other:?}"),
    }
}

fn record_count(store: &TaskRecordStore) -> usize {
    store.list().expect("list").records.len()
}

fn manager_over(
    project: &Project,
    runner: &Arc<FakeRunner>,
    saver: Option<Arc<CapturingSaver>>,
) -> TaskManager {
    let mut options = TaskManagerOptions::new(
        project.store(),
        ManagedRunners {
            in_process: Arc::clone(runner) as Arc<dyn ManagedRunner>,
            process: Arc::clone(runner) as Arc<dyn ManagedRunner>,
        },
        super::fakes::category_planner(&[]),
        project.cwd(),
    );
    options.config = config(5, 1);
    options.record_saver = saver.map(|saver| saver as Arc<dyn TaskRecordSaver + Send + Sync>);
    create_task_manager(options)
}

/// Delegates to the real store and remembers the owner every save carried.
struct CapturingSaver {
    inner: TaskRecordStore,
    claimed_owner: Mutex<Option<DagTaskOwner>>,
}

impl TaskRecordSaver for CapturingSaver {
    fn save(&self, record: &TaskRecord) -> Result<(), StoreError> {
        *lock(&self.claimed_owner) = record.owner.clone();
        self.inner.save(record)
    }
}

#[test]
fn given_new_dag_owner_when_started_then_initial_claim_persists_owner_before_launch() {
    let project = Project::new();
    let saver = Arc::new(CapturingSaver {
        inner: project.store(),
        claimed_owner: Mutex::new(None),
    });
    let runner = FakeRunner::new();
    let manager = manager_over(&project, &runner, Some(Arc::clone(&saver)));

    let result = manager.start_owned(&base_spec(), &owner());

    let (task_id, reused) = started_fresh(&result);
    assert!(!reused);
    assert_eq!(*lock(&saver.claimed_owner), Some(owner()));
    let persisted = project
        .store()
        .load(task_id)
        .expect("load")
        .expect("record");
    assert_eq!(persisted.owner, Some(owner()));
    assert_eq!(runner.specs().len(), 1);
}

#[test]
fn given_overlapping_starts_for_one_owner_when_first_awaits_launch_then_owner_lock_serializes_second()
 {
    let project = Project::new();
    let runner = FakeRunner::new();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let hook_gate = Arc::clone(&gate);
    *lock(&runner.hook) = Some(Arc::new(move |_spec, _call| {
        let (open, changed) = &*hook_gate;
        let mut open = lock(open);
        while !*open {
            open = changed
                .wait(open)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        None
    }));
    let first_manager = manager_over(&project, &runner, None);
    let second_manager = manager_over(&project, &runner, None);
    let owner_key = format!("dag\0{}\0{}", owner().run_id, owner().node_id);
    let hex: String = Sha256::digest(owner_key.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let lock_path = project
        .store()
        .state_dir()
        .join("owner-locks")
        .join(format!("{hex}.lock"));

    let first = thread::spawn(move || first_manager.start_owned(&base_spec(), &owner()));
    wait_until("first launch blocked", || runner.started_count() == 1);
    let second = thread::spawn(move || second_manager.start_owned(&base_spec(), &owner()));

    assert!(lock_path.exists(), "{}", lock_path.display());
    assert!(!second.is_finished());
    *lock(&gate.0) = true;
    gate.1.notify_all();
    notify();
    let first = first.join().expect("first");
    let second = second.join().expect("second");
    let (first_id, first_reused) = started_fresh(&first);
    let (second_id, second_reused) = started_fresh(&second);
    assert!(!first_reused);
    assert_eq!(first_id, second_id);
    assert!(second_reused);
    assert_eq!(runner.started_count(), 1);
}

#[test]
fn given_existing_owner_with_same_fingerprint_when_started_again_then_reuses_one_task() {
    let harness = make_manager(HarnessOptions::default());
    let first = harness.manager.start_owned(&base_spec(), &owner());
    let (first_id, _) = started_fresh(&first);

    let second = harness.manager.start_owned(&base_spec(), &owner());

    let (second_id, reused) = started_fresh(&second);
    assert!(reused);
    assert_eq!(second_id, first_id);
    assert_eq!(record_count(&harness.store), 1);
    assert_eq!(harness.in_process.started_count(), 1);
    assert_eq!(
        harness
            .manager
            .find_owned_task("run-1", "node-1")
            .map(|record| record.task_id),
        Some(first_id.to_string())
    );
}

#[test]
fn given_existing_owner_with_different_fingerprint_when_started_again_then_owner_conflict() {
    let harness = make_manager(HarnessOptions::default());
    let first = harness.manager.start_owned(&base_spec(), &owner());
    let (first_id, _) = started_fresh(&first);

    let conflict = harness.manager.start_owned(
        &base_spec(),
        &DagTaskOwner {
            fingerprint: "fingerprint-2".to_string(),
            ..owner()
        },
    );

    assert_eq!(
        conflict,
        OwnedStartResult::OwnerConflict {
            task_id: first_id.to_string(),
            existing_fingerprint: "fingerprint-1".to_string(),
            requested_fingerprint: "fingerprint-2".to_string(),
        }
    );
    assert_eq!(record_count(&harness.store), 1);
    assert_eq!(harness.in_process.started_count(), 1);
}

#[test]
fn given_caller_journal_lost_after_dispatch_when_fresh_manager_starts_same_owner_then_recovers_one_task()
 {
    let project = Project::new();
    let first_harness = make_manager(HarnessOptions {
        project: Some(project.clone()),
        ..HarnessOptions::default()
    });
    let first = first_harness.manager.start_owned(&base_spec(), &owner());
    let (first_id, _) = started_fresh(&first);

    let recovered_harness = make_manager(HarnessOptions {
        project: Some(project),
        ..HarnessOptions::default()
    });
    let recovered = recovered_harness
        .manager
        .start_owned(&base_spec(), &owner());

    let (recovered_id, reused) = started_fresh(&recovered);
    assert!(reused);
    assert_eq!(recovered_id, first_id);
    assert_eq!(record_count(&recovered_harness.store), 1);
    assert_eq!(recovered_harness.in_process.started_count(), 0);
}

#[test]
fn given_depth_residency_and_concurrency_gates_when_owned_starts_attempted_then_manager_outcomes_hold()
 {
    let depth = make_manager(HarnessOptions {
        config: Some(config(1, 1)),
        ..HarnessOptions::default()
    });
    let residency = make_manager(HarnessOptions {
        customize: Some(Box::new(|options: &mut TaskManagerOptions| {
            options.admit = Some(Arc::new(|_parent: &str| SpawnAdmission::Rejected {
                message: "cap reached".to_string(),
            }));
        })),
        ..HarnessOptions::default()
    });
    let queued = make_manager(HarnessOptions {
        config: Some(config(1, 1)),
        ..HarnessOptions::default()
    });

    let depth_result = depth.manager.start_owned(
        &ManagerStartSpec {
            depth: 2,
            ..base_spec()
        },
        &owner(),
    );
    let residency_result = residency.manager.start_owned(&base_spec(), &owner());
    let running = queued.manager.start_owned(
        &ManagerStartSpec {
            name: Some("running".to_string()),
            ..base_spec()
        },
        &owner(),
    );
    let pending = queued.manager.start_owned(
        &ManagerStartSpec {
            name: Some("pending".to_string()),
            ..base_spec()
        },
        &DagTaskOwner {
            node_id: "node-2".to_string(),
            fingerprint: "fingerprint-2".to_string(),
            ..owner()
        },
    );

    assert!(matches!(
        depth_result,
        OwnedStartResult::NotStarted(StartResult::DepthDenied { .. })
    ));
    assert_eq!(record_count(&depth.store), 0);
    assert!(matches!(
        residency_result,
        OwnedStartResult::NotStarted(StartResult::ResidencyDenied { .. })
    ));
    assert_eq!(record_count(&residency.store), 0);
    assert_eq!(running.kind(), "started");
    match pending {
        OwnedStartResult::Started { task, reused } => {
            assert!(!reused);
            assert_eq!(task.status, TaskStatus::Pending);
            assert_eq!(task.queue_position, Some(1));
        }
        other => panic!("expected pending start, got {other:?}"),
    }
    assert_eq!(record_count(&queued.store), 2);
}
