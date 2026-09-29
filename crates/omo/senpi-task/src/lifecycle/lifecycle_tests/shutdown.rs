//! `lifecycle/shutdown.test.ts`.

use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::lifecycle::{LifecycleDeps, SuspendInput, SuspendSummary, create_task_lifecycle};
use crate::state::{ResidencyState::*, TaskStatus::*};

const FOREIGN_PID: i64 = 9_999_999;

fn host() -> i64 {
    i64::from(std::process::id())
}

fn input() -> SuspendInput {
    SuspendInput {
        parent_session_id: "parent-1".to_string(),
        reason: "quit".to_string(),
    }
}

fn summary(in_process: usize, rpc: usize, pending: usize, disposed: usize) -> SuspendSummary {
    SuspendSummary {
        suspended_in_process: in_process,
        suspended_rpc: rpc,
        suspended_pending: pending,
        disposed,
        failures: Vec::new(),
    }
}

fn resident<'a>(task_id: &'a str, status: TaskStatus) -> Seed<'a> {
    Seed {
        task_id,
        status: Some(status),
        residency_state: Some(Resident),
        execution_mode: Some("in-process"),
        host_pid: Some(host()),
        ..Seed::default()
    }
}

fn index(order: &[String], entry: &str) -> usize {
    order
        .iter()
        .position(|candidate| candidate == entry)
        .unwrap_or_else(|| panic!("{entry} missing from {order:?}"))
}

fn last_event(store: &TaskRecordStore, task_id: &str) -> Option<String> {
    read_events(store, task_id).pop()
}

fn load(store: &TaskRecordStore, task_id: &str) -> TaskRecord {
    store.load(task_id).expect("load").expect("record")
}

fn in_process(task_id: &str, order: &CallLog, options: HandleOptions) -> Arc<FakeHandle> {
    fake_handle(task_id, ResidentKind::InProcess, order, options)
}

type Dequeued = Arc<Mutex<Vec<String>>>;

fn with_dequeue(deps: &mut LifecycleDeps) -> Dequeued {
    let dequeued: Dequeued = Arc::default();
    let sink = Arc::clone(&dequeued);
    deps.dequeue_pending = Some(Arc::new(move |task_id: &str| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(task_id.to_string());
    }));
    dequeued
}

fn suspend(deps: LifecycleDeps) -> SuspendSummary {
    create_task_lifecycle(deps)
        .suspend_on_session_shutdown(&input())
        .expect("suspend")
}

#[test]
fn in_process_running_resident_suspends_persisted_only() {
    let temp = temp_store();
    seed_record(
        &temp.store,
        Seed {
            run_epoch: Some(3),
            ..resident("st_000000f0", Running)
        },
    );
    let order = call_log();
    let registry = Arc::new(FakeRegistry::with_order(&order));
    registry.add(in_process("st_000000f0", &order, HandleOptions::default()));
    let result = lifecycle(&temp.store, &registry, default_settings())
        .suspend_on_session_shutdown(&input())
        .expect("suspend");
    let record = load(&temp.store, "st_000000f0");
    assert_eq!(record.status, Running);
    assert_eq!(record.residency_state, PersistedOnly);
    assert_eq!(record.host_pid, None);
    assert_eq!(record.notification.run_epoch, 3);
    let order = calls(&order);
    assert!(index(&order, "forget:st_000000f0") < index(&order, "abort:st_000000f0"));
    assert!(index(&order, "abort:st_000000f0") < index(&order, "dispose:st_000000f0"));
    assert_eq!(
        last_event(&temp.store, "st_000000f0").as_deref(),
        Some("suspended")
    );
    assert_eq!(result, summary(1, 0, 0, 0));
}

#[test]
fn rpc_running_resident_suspends_rpc_detached_keeping_pid() {
    let temp = temp_store();
    seed_record(
        &temp.store,
        Seed {
            execution_mode: Some("process"),
            pid: Some(55),
            ..resident("st_000000f1", Running)
        },
    );
    let order = call_log();
    let registry = Arc::new(FakeRegistry::with_order(&order));
    registry.add(fake_handle(
        "st_000000f1",
        ResidentKind::Rpc,
        &order,
        HandleOptions {
            pid: Some(55),
            ..HandleOptions::default()
        },
    ));
    let result = lifecycle(&temp.store, &registry, default_settings())
        .suspend_on_session_shutdown(&SuspendInput {
            parent_session_id: "parent-1".to_string(),
            reason: "reload".to_string(),
        })
        .expect("suspend");
    let record = load(&temp.store, "st_000000f1");
    assert_eq!(record.status, Running);
    assert_eq!(record.residency_state, RpcDetached);
    assert_eq!(record.pid, Some(55));
    assert_eq!(record.host_pid, None);
    let order = calls(&order);
    assert!(index(&order, "abort:st_000000f1") < index(&order, "terminate:st_000000f1"));
    assert!(index(&order, "terminate:st_000000f1") < index(&order, "dispose:st_000000f1"));
    assert_eq!(
        last_event(&temp.store, "st_000000f1").as_deref(),
        Some("suspended")
    );
    assert_eq!(result, summary(0, 1, 0, 0));
}

#[test]
fn resident_of_other_session_is_untouched() {
    let temp = temp_store();
    seed_record(
        &temp.store,
        Seed {
            parent_session_id: Some("session-b"),
            ..resident("st_000000f2", Running)
        },
    );
    let order = call_log();
    let registry = Arc::new(FakeRegistry::with_order(&order));
    registry.add(in_process("st_000000f2", &order, HandleOptions::default()));
    let result = lifecycle(&temp.store, &registry, default_settings())
        .suspend_on_session_shutdown(&input())
        .expect("suspend");
    let record = load(&temp.store, "st_000000f2");
    assert_eq!(record.residency_state, Resident);
    assert_eq!(record.host_pid, Some(host()));
    assert!(registry.get("st_000000f2").is_some());
    assert!(registry.forgotten().is_empty());
    assert!(read_events(&temp.store, "st_000000f2").is_empty());
    assert_eq!(result, summary(0, 0, 0, 0));
}

#[test]
fn resident_owned_by_foreign_host_is_untouched() {
    let temp = temp_store();
    seed_record(
        &temp.store,
        Seed {
            host_pid: Some(FOREIGN_PID),
            ..resident("st_000000f3", Running)
        },
    );
    let order = call_log();
    let registry = Arc::new(FakeRegistry::with_order(&order));
    registry.add(in_process("st_000000f3", &order, HandleOptions::default()));
    let result = lifecycle(&temp.store, &registry, default_settings())
        .suspend_on_session_shutdown(&input())
        .expect("suspend");
    let record = load(&temp.store, "st_000000f3");
    assert_eq!(record.residency_state, Resident);
    assert_eq!(record.host_pid, Some(FOREIGN_PID));
    assert!(registry.forgotten().is_empty());
    assert!(read_events(&temp.store, "st_000000f3").is_empty());
    assert_eq!(result, summary(0, 0, 0, 0));
}

#[test]
fn cancelled_resident_is_disposed_not_suspended() {
    let temp = temp_store();
    seed_record(&temp.store, resident("st_000000f4", Cancelled));
    let order = call_log();
    let registry = Arc::new(FakeRegistry::with_order(&order));
    registry.add(in_process("st_000000f4", &order, HandleOptions::default()));
    let result = lifecycle(&temp.store, &registry, default_settings())
        .suspend_on_session_shutdown(&input())
        .expect("suspend");
    let record = load(&temp.store, "st_000000f4");
    assert_eq!(record.status, Cancelled);
    assert_eq!(record.residency_state, Disposed);
    assert_eq!(registry.forgotten(), ["st_000000f4"]);
    assert_eq!(
        last_event(&temp.store, "st_000000f4").as_deref(),
        Some("destroyed")
    );
    assert!(!read_events(&temp.store, "st_000000f4").contains(&"suspended".to_string()));
    assert_eq!(result, summary(0, 0, 0, 1));
}

#[test]
fn completed_resident_suspends_preserving_status() {
    let temp = temp_store();
    seed_record(&temp.store, resident("st_000000f5", Completed));
    let order = call_log();
    let registry = Arc::new(FakeRegistry::with_order(&order));
    registry.add(in_process("st_000000f5", &order, HandleOptions::default()));
    let result = lifecycle(&temp.store, &registry, default_settings())
        .suspend_on_session_shutdown(&input())
        .expect("suspend");
    let record = load(&temp.store, "st_000000f5");
    assert_eq!(record.status, Completed);
    assert_eq!(record.residency_state, PersistedOnly);
    assert_eq!(record.host_pid, None);
    assert_eq!(
        last_event(&temp.store, "st_000000f5").as_deref(),
        Some("suspended")
    );
    assert_eq!(result, summary(1, 0, 0, 0));
}

#[test]
fn pending_record_without_handle_is_dequeued_and_suspended() {
    let temp = temp_store();
    seed_record(
        &temp.store,
        Seed {
            run_epoch: Some(2),
            ..resident("st_000000f6", Pending)
        },
    );
    let mut deps = LifecycleDeps::new(
        temp.store.clone(),
        Arc::new(FakeRegistry::default()),
        default_settings(),
    );
    let dequeued = with_dequeue(&mut deps);
    let result = suspend(deps);
    let record = load(&temp.store, "st_000000f6");
    assert_eq!(record.status, Pending);
    assert_eq!(record.residency_state, PersistedOnly);
    assert_eq!(record.host_pid, None);
    assert_eq!(record.notification.run_epoch, 2);
    assert_eq!(
        *dequeued.lock().unwrap_or_else(PoisonError::into_inner),
        ["st_000000f6"]
    );
    assert_eq!(
        last_event(&temp.store, "st_000000f6").as_deref(),
        Some("suspended")
    );
    assert_eq!(result, summary(0, 0, 1, 0));
}

#[test]
fn pending_record_of_other_session_is_never_dequeued() {
    let temp = temp_store();
    seed_record(
        &temp.store,
        Seed {
            parent_session_id: Some("session-b"),
            ..resident("st_000000f7", Pending)
        },
    );
    let mut deps = LifecycleDeps::new(
        temp.store.clone(),
        Arc::new(FakeRegistry::default()),
        default_settings(),
    );
    let dequeued = with_dequeue(&mut deps);
    let result = suspend(deps);
    let record = load(&temp.store, "st_000000f7");
    assert_eq!(record.status, Pending);
    assert_eq!(record.residency_state, Resident);
    assert_eq!(record.host_pid, Some(host()));
    assert!(
        dequeued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    );
    assert!(read_events(&temp.store, "st_000000f7").is_empty());
    assert_eq!(result, summary(0, 0, 0, 0));
}

#[test]
fn killed_resident_is_disposed_never_persisted_only() {
    let temp = temp_store();
    seed_record(
        &temp.store,
        Seed {
            killed: true,
            ..resident("st_000000f8", Error)
        },
    );
    let order = call_log();
    let registry = Arc::new(FakeRegistry::with_order(&order));
    registry.add(in_process("st_000000f8", &order, HandleOptions::default()));
    let result = lifecycle(&temp.store, &registry, default_settings())
        .suspend_on_session_shutdown(&input())
        .expect("suspend");
    let record = load(&temp.store, "st_000000f8");
    assert_eq!(record.status, Error);
    assert_eq!(record.killed, Some(true));
    assert_eq!(record.residency_state, Disposed);
    assert_eq!(
        last_event(&temp.store, "st_000000f8").as_deref(),
        Some("destroyed")
    );
    assert!(!read_events(&temp.store, "st_000000f8").contains(&"suspended".to_string()));
    assert_eq!(result, summary(0, 0, 0, 1));
}

#[test]
fn resume_children_false_runs_legacy_dispose() {
    let temp = temp_store();
    seed_record(&temp.store, resident("st_000000f9", Running));
    seed_record(&temp.store, resident("st_000000fa", Pending));
    let order = call_log();
    let registry = Arc::new(FakeRegistry::with_order(&order));
    registry.add(in_process("st_000000f9", &order, HandleOptions::default()));
    let mut deps = LifecycleDeps::new(
        temp.store.clone(),
        registry.clone(),
        settings(json!({ "resume_children": false })),
    );
    let dequeued = with_dequeue(&mut deps);
    let result = suspend(deps);
    assert_eq!(load(&temp.store, "st_000000f9").residency_state, Disposed);
    assert_eq!(
        last_event(&temp.store, "st_000000f9").as_deref(),
        Some("destroyed")
    );
    assert!(!read_events(&temp.store, "st_000000f9").contains(&"suspended".to_string()));
    assert_eq!(registry.forgotten(), ["st_000000f9"]);
    let pending = load(&temp.store, "st_000000fa");
    assert_eq!(pending.status, Pending);
    assert_eq!(pending.residency_state, Resident);
    assert_eq!(pending.host_pid, Some(host()));
    assert!(
        dequeued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    );
    assert!(read_events(&temp.store, "st_000000fa").is_empty());
    assert_eq!(result, summary(0, 0, 0, 1));
}

#[test]
fn one_failing_dispose_does_not_block_other_children() {
    let temp = temp_store();
    seed_record(&temp.store, resident("st_000000fb", Running));
    seed_record(&temp.store, resident("st_000000fc", Running));
    let order = call_log();
    let registry = Arc::new(FakeRegistry::with_order(&order));
    registry.add(in_process(
        "st_000000fb",
        &order,
        HandleOptions {
            dispose_rejects: true,
            ..HandleOptions::default()
        },
    ));
    registry.add(in_process("st_000000fc", &order, HandleOptions::default()));
    let result = lifecycle(&temp.store, &registry, default_settings())
        .suspend_on_session_shutdown(&input())
        .expect("suspend");
    assert_eq!(load(&temp.store, "st_000000fb").residency_state, Resident);
    assert!(read_events(&temp.store, "st_000000fb").is_empty());
    assert_eq!(
        load(&temp.store, "st_000000fc").residency_state,
        PersistedOnly
    );
    assert_eq!(
        last_event(&temp.store, "st_000000fc").as_deref(),
        Some("suspended")
    );
    assert_eq!(result.suspended_in_process, 1);
    assert_eq!(result.failures.len(), 1);
    assert_eq!(result.failures[0].task_id, "st_000000fb");
    assert!(result.failures[0].error.contains("dispose exploded"));
}

#[test]
fn rejected_abort_still_suspends_without_failure() {
    let temp = temp_store();
    seed_record(&temp.store, resident("st_000000fd", Running));
    let order = call_log();
    let registry = Arc::new(FakeRegistry::with_order(&order));
    let handle = in_process(
        "st_000000fd",
        &order,
        HandleOptions {
            abort_rejects: true,
            ..HandleOptions::default()
        },
    );
    registry.add(handle.clone());
    let result = lifecycle(&temp.store, &registry, default_settings())
        .suspend_on_session_shutdown(&input())
        .expect("suspend");
    assert!(handle.did("dispose"));
    assert_eq!(
        load(&temp.store, "st_000000fd").residency_state,
        PersistedOnly
    );
    assert_eq!(
        last_event(&temp.store, "st_000000fd").as_deref(),
        Some("suspended")
    );
    assert_eq!(result, summary(1, 0, 0, 0));
}

#[test]
fn no_children_yields_zero_summary() {
    let temp = temp_store();
    let result = lifecycle(&temp.store, &Arc::default(), default_settings())
        .suspend_on_session_shutdown(&input())
        .expect("suspend");
    assert_eq!(result, summary(0, 0, 0, 0));
}
