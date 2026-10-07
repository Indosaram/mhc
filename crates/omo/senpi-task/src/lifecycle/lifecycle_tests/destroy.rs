//! `lifecycle/destroy.test.ts`.

use std::sync::{Arc, Mutex, PoisonError};

use super::*;
use crate::lifecycle::{
    DestroyCause, HostSessionCloseRequest, HostSessionCloser, HostSessionLiveness, HostSessionProbe,
    LifecycleDeps, create_task_lifecycle,
};
use crate::state::{HostSessionIdentity, ResidencyState::*, RunnerKind, TaskStatus::*};

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

struct FakeHostSessionProbe {
    live: bool,
}

impl HostSessionProbe for FakeHostSessionProbe {
    fn daemon_alive(&self, _host_session: &HostSessionIdentity) -> bool {
        self.live
    }
    fn session_live(&self, _host_session: &HostSessionIdentity) -> bool {
        self.live
    }
    fn session_liveness(&self, _host_session: &HostSessionIdentity) -> HostSessionLiveness {
        if self.live {
            HostSessionLiveness::Live
        } else {
            HostSessionLiveness::Gone
        }
    }
    fn refresh(&self, _socket: Option<&str>) {}
}

fn host_identity(session_path: &str) -> HostSessionIdentity {
    HostSessionIdentity {
        socket: "/tmp/host.sock".to_string(),
        routing_id: "rpc-1".to_string(),
        session_path: session_path.to_string(),
        instance_id: "inst-1".to_string(),
        daemon_pid: Some(4242),
    }
}

#[test]
fn host_session_orphan_is_closed_through_the_close_writer_never_signalled() {
    let temp = temp_store();
    seed_record(
        &temp.store,
        Seed {
            task_id: "st_0000001a",
            status: Some(Interrupted),
            residency_state: Some(Resident),
            execution_mode: Some("process"),
            runner_kind: Some(RunnerKind::HostSession),
            host_session: Some(host_identity("/tmp/host.jsonl")),
            ..Seed::default()
        },
    );
    let closed = Arc::new(Mutex::new(Vec::new()));
    let closer: HostSessionCloser = {
        let closed = Arc::clone(&closed);
        Arc::new(move |request: &HostSessionCloseRequest| {
            closed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(request.host_session.session_path.clone());
            Ok(())
        })
    };
    let registry = Arc::new(FakeRegistry::default());
    let mut deps = LifecycleDeps::new(temp.store.clone(), registry.clone(), default_settings());
    deps.host_session_probe = Some(Arc::new(FakeHostSessionProbe { live: true }));
    deps.host_session_close = Some(closer);
    create_task_lifecycle(deps)
        .destroy_resident_task("st_0000001a", DestroyCause::ReconcileLost)
        .expect("destroy");
    assert_eq!(
        *closed.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["/tmp/host.jsonl".to_string()]
    );
    assert!(read_events(&temp.store, "st_0000001a").contains(&"host_session_closed".to_string()));
}
