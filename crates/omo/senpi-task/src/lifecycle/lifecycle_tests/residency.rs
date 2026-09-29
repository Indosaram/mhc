//! `lifecycle/residency.test.ts`.

use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::lifecycle::AdmissionResult;
use crate::state::{ResidencyState::*, TaskStatus::*};

fn seed(temp: &TempStore, task_id: &str, parent: &str, status: TaskStatus, offset: i64) {
    seed_record(
        &temp.store,
        Seed {
            task_id,
            parent_session_id: Some(parent),
            status: Some(status),
            residency_state: Some(Resident),
            updated_at: Some(iso(offset)),
            ..Seed::default()
        },
    );
}

fn cap(max: u64) -> TaskSettings {
    settings(json!({ "residency_max_children": max }))
}

fn evicted(task_id: &str) -> AdmissionResult {
    AdmissionResult::Evicted {
        evicted_task_id: task_id.to_string(),
    }
}

#[test]
fn below_cap_is_admitted() {
    let temp = temp_store();
    seed(&temp, "st_00000001", "parent-1", Running, 0);
    let result = lifecycle(&temp.store, &Arc::default(), cap(8))
        .admit_resident("parent-1")
        .expect("admit");
    assert_eq!(result, AdmissionResult::Admitted);
}

#[test]
fn full_session_evicts_oldest_idle_terminal() {
    let temp = temp_store();
    let registry = Arc::new(FakeRegistry::default());
    for (index, id) in ["st_000000a0", "st_000000a1"].into_iter().enumerate() {
        seed(
            &temp,
            id,
            "parent-1",
            Completed,
            i64::try_from(index).expect("index"),
        );
        registry.add(fake_handle(
            id,
            ResidentKind::InProcess,
            &call_log(),
            HandleOptions::default(),
        ));
    }
    let result = lifecycle(&temp.store, &registry, cap(2))
        .admit_resident("parent-1")
        .expect("admit");
    assert_eq!(result, evicted("st_000000a0"));
    assert_eq!(residency(&temp.store, "st_000000a0"), Some(Evicted));
    assert_eq!(residency(&temp.store, "st_000000a1"), Some(Resident));
}

#[test]
fn pending_send_skips_oldest_terminal() {
    let temp = temp_store();
    let registry = Arc::new(FakeRegistry::default());
    seed(&temp, "st_000000b0", "parent-1", Completed, 0);
    seed(&temp, "st_000000b1", "parent-1", Completed, 10);
    registry.add(fake_handle(
        "st_000000b0",
        ResidentKind::InProcess,
        &call_log(),
        HandleOptions::default(),
    ));
    registry.add(fake_handle(
        "st_000000b1",
        ResidentKind::InProcess,
        &call_log(),
        HandleOptions::default(),
    ));
    registry.mark_pending("st_000000b0");
    let result = lifecycle(&temp.store, &registry, cap(2))
        .admit_resident("parent-1")
        .expect("admit");
    assert_eq!(result, evicted("st_000000b1"));
    assert_eq!(residency(&temp.store, "st_000000b0"), Some(Resident));
}

#[test]
fn all_running_residents_reject_without_eviction() {
    let temp = temp_store();
    let order = call_log();
    let registry = Arc::new(FakeRegistry::default());
    for (index, id) in ["st_000000c0", "st_000000c1"].into_iter().enumerate() {
        seed(
            &temp,
            id,
            "parent-1",
            Running,
            i64::try_from(index).expect("index"),
        );
        registry.add(fake_handle(
            id,
            ResidentKind::InProcess,
            &order,
            HandleOptions::default(),
        ));
    }
    let result = lifecycle(&temp.store, &registry, cap(2))
        .admit_resident("parent-1")
        .expect("admit");
    let AdmissionResult::Rejected(error) = result else {
        panic!("expected rejection, got {result:?}");
    };
    assert!(error.to_string().contains("st_000000c0"));
    assert_eq!(error.residents.len(), 2);
    assert!(calls(&order).is_empty());
}

#[test]
fn cap_is_per_parent_session() {
    let temp = temp_store();
    seed(&temp, "st_000000d0", "other", Running, 0);
    seed(&temp, "st_000000d1", "other", Running, 0);
    let result = lifecycle(&temp.store, &Arc::default(), cap(2))
        .admit_resident("parent-1")
        .expect("admit");
    assert_eq!(result, AdmissionResult::Admitted);
}

#[test]
fn lost_residents_are_evicted_first() {
    let temp = temp_store();
    seed(&temp, "st_000000e0", "parent-1", Lost, 0);
    seed(&temp, "st_000000e1", "parent-1", Running, 10);
    let result = lifecycle(&temp.store, &Arc::default(), cap(2))
        .admit_resident("parent-1")
        .expect("admit");
    assert_eq!(result, evicted("st_000000e0"));
    assert_eq!(residency(&temp.store, "st_000000e0"), Some(Evicted));
    assert_eq!(residency(&temp.store, "st_000000e1"), Some(Resident));
}

#[test]
fn cancelled_resident_is_evicted_like_any_terminal() {
    let temp = temp_store();
    seed(&temp, "st_000000f0", "parent-1", Cancelled, 0);
    seed(&temp, "st_000000f1", "parent-1", Running, 10);
    let result = lifecycle(&temp.store, &Arc::default(), cap(2))
        .admit_resident("parent-1")
        .expect("admit");
    assert_eq!(result, evicted("st_000000f0"));
}
