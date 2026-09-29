//! `lifecycle/destroy.test.ts`.

use std::sync::Arc;

use super::*;
use crate::lifecycle::DestroyCause;
use crate::state::{ResidencyState::*, TaskStatus::*};

fn seeded(task_id: &str, status: TaskStatus, mode: &str) -> (TempStore, Arc<FakeRegistry>) {
    let temp = temp_store();
    seed_record(
        &temp.store,
        Seed {
            task_id,
            status: Some(status),
            residency_state: Some(Resident),
            execution_mode: Some(mode),
            ..Seed::default()
        },
    );
    (temp, Arc::new(FakeRegistry::default()))
}

#[test]
fn in_process_cancel_aborts_before_dispose_and_marks_disposed() {
    let (temp, registry) = seeded("st_0000000a", Cancelled, "in-process");
    let order = call_log();
    let handle = fake_handle(
        "st_0000000a",
        ResidentKind::InProcess,
        &order,
        HandleOptions::default(),
    );
    registry.add(handle.clone());
    lifecycle(&temp.store, &registry, default_settings())
        .destroy_resident_task("st_0000000a", DestroyCause::Cancel)
        .expect("destroy");
    assert_eq!(calls(&order), ["abort:st_0000000a", "dispose:st_0000000a"]);
    assert!(!handle.did("terminate"));
    assert_eq!(residency(&temp.store, "st_0000000a"), Some(Disposed));
    assert!(registry.forgotten().contains(&"st_0000000a".to_string()));
    assert!(read_events(&temp.store, "st_0000000a").contains(&"destroyed".to_string()));
}

#[test]
fn cancel_without_abort_disposes_without_aborting() {
    let (temp, registry) = seeded("st_00000007", Cancelled, "in-process");
    let order = call_log();
    registry.add(fake_handle(
        "st_00000007",
        ResidentKind::InProcess,
        &order,
        HandleOptions {
            abort_rejects: true,
            ..HandleOptions::default()
        },
    ));
    lifecycle(&temp.store, &registry, default_settings())
        .destroy_resident_task("st_00000007", DestroyCause::CancelWithoutAbort)
        .expect("destroy");
    assert_eq!(calls(&order), ["dispose:st_00000007"]);
    assert_eq!(residency(&temp.store, "st_00000007"), Some(Disposed));
    assert!(registry.forgotten().contains(&"st_00000007".to_string()));
}

#[test]
fn rejected_abort_still_disposes() {
    let (temp, registry) = seeded("st_0000000e", Cancelled, "in-process");
    let order = call_log();
    let handle = fake_handle(
        "st_0000000e",
        ResidentKind::InProcess,
        &order,
        HandleOptions {
            abort_rejects: true,
            ..HandleOptions::default()
        },
    );
    registry.add(handle.clone());
    lifecycle(&temp.store, &registry, default_settings())
        .destroy_resident_task("st_0000000e", DestroyCause::Cancel)
        .expect("destroy");
    assert_eq!(calls(&order), ["abort:st_0000000e", "dispose:st_0000000e"]);
    assert!(handle.did("dispose"));
    assert_eq!(residency(&temp.store, "st_0000000e"), Some(Disposed));
    assert!(registry.forgotten().contains(&"st_0000000e".to_string()));
}

#[test]
fn fallback_handoff_keeps_manager_ownership_registered() {
    let (temp, registry) = seeded("st_0000000f", Running, "process");
    let order = call_log();
    registry.add(fake_handle(
        "st_0000000f",
        ResidentKind::Rpc,
        &order,
        HandleOptions {
            pid: Some(4242),
            ..HandleOptions::default()
        },
    ));
    lifecycle(&temp.store, &registry, default_settings())
        .destroy_resident_task("st_0000000f", DestroyCause::FallbackHandoff)
        .expect("destroy");
    assert_eq!(
        calls(&order),
        ["terminate:st_0000000f", "dispose:st_0000000f"]
    );
    assert_eq!(residency(&temp.store, "st_0000000f"), Some(Resident));
    assert!(!registry.forgotten().contains(&"st_0000000f".to_string()));
    assert!(!read_events(&temp.store, "st_0000000f").contains(&"destroyed".to_string()));
}

#[test]
fn rpc_resident_terminates_then_disposes() {
    let (temp, registry) = seeded("st_0000000b", Cancelled, "process");
    let order = call_log();
    registry.add(fake_handle(
        "st_0000000b",
        ResidentKind::Rpc,
        &order,
        HandleOptions {
            pid: Some(4242),
            ..HandleOptions::default()
        },
    ));
    lifecycle(&temp.store, &registry, default_settings())
        .destroy_resident_task("st_0000000b", DestroyCause::Cancel)
        .expect("destroy");
    assert_eq!(
        calls(&order),
        ["terminate:st_0000000b", "dispose:st_0000000b"]
    );
    assert_eq!(residency(&temp.store, "st_0000000b"), Some(Disposed));
}

#[test]
fn evicted_terminal_resident_records_evicted_event() {
    let (temp, registry) = seeded("st_0000000c", Completed, "in-process");
    registry.add(fake_handle(
        "st_0000000c",
        ResidentKind::InProcess,
        &call_log(),
        HandleOptions::default(),
    ));
    lifecycle(&temp.store, &registry, default_settings())
        .destroy_resident_task("st_0000000c", DestroyCause::Evict)
        .expect("destroy");
    assert_eq!(residency(&temp.store, "st_0000000c"), Some(Evicted));
    assert!(read_events(&temp.store, "st_0000000c").contains(&"evicted".to_string()));
}

#[test]
fn destroy_without_handle_is_idempotent() {
    let (temp, registry) = seeded("st_0000000d", Cancelled, "in-process");
    let lifecycle = lifecycle(&temp.store, &registry, default_settings());
    lifecycle
        .destroy_resident_task("st_0000000d", DestroyCause::Cancel)
        .expect("first destroy");
    lifecycle
        .destroy_resident_task("st_0000000d", DestroyCause::Cancel)
        .expect("second destroy");
    assert_eq!(residency(&temp.store, "st_0000000d"), Some(Disposed));
}
