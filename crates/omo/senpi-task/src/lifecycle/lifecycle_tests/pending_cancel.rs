//! `lifecycle/pending-cancel.ts`: finishing a cancel accepted while the child was unreachable.

use std::sync::{Arc, Mutex, PoisonError};

use super::*;
use crate::lifecycle::pending_cancel::finish_pending_cancel;
use crate::lifecycle::{
    HostSessionCloseError, HostSessionCloseRequest, HostSessionCloser, HostSessionLiveness,
    HostSessionProbe, LifecycleContext, LifecycleDeps, ReconcileOutcomeKind, resolve_context,
};
use crate::state::{
    CancelRequest, HostSessionIdentity, ResidencyState::*, RunnerKind, TaskStatus::*,
};

struct FakeProbe {
    liveness: Mutex<HostSessionLiveness>,
}

impl HostSessionProbe for FakeProbe {
    fn daemon_alive(&self, _host_session: &HostSessionIdentity) -> bool {
        true
    }
    fn session_live(&self, host_session: &HostSessionIdentity) -> bool {
        self.session_liveness(host_session) == HostSessionLiveness::Live
    }
    fn session_liveness(&self, _host_session: &HostSessionIdentity) -> HostSessionLiveness {
        *self.liveness.lock().unwrap_or_else(PoisonError::into_inner)
    }
    fn refresh(&self, _socket: Option<&str>) {}
}

fn identity() -> HostSessionIdentity {
    HostSessionIdentity {
        socket: "/tmp/host.sock".to_string(),
        routing_id: "rpc-1".to_string(),
        session_path: "/tmp/pending-cancel.jsonl".to_string(),
        instance_id: "inst-1".to_string(),
        daemon_pid: Some(4242),
        effect_seq: None,
        adopted_entry_id: None,
    }
}

struct Fixture {
    temp: TempStore,
    context: LifecycleContext,
    closed: Arc<Mutex<Vec<String>>>,
}

fn fixture(
    task_id: &str,
    hosted: bool,
    liveness: HostSessionLiveness,
    close_confirms: bool,
    reason: Option<&str>,
) -> Fixture {
    let temp = temp_store();
    seed_record(
        &temp.store,
        Seed {
            task_id,
            status: Some(Running),
            residency_state: Some(Resident),
            execution_mode: Some("process"),
            runner_kind: hosted.then_some(RunnerKind::HostSession),
            host_session: hosted.then(identity),
            ..Seed::default()
        },
    );
    let reason = reason.map(str::to_string);
    temp.store
        .mutate(task_id, |fresh| crate::state::TaskRecord {
            cancel_requested: Some(CancelRequest {
                requested_at: "2026-07-06T00:01:00.000Z".to_string(),
                reason: reason.clone(),
            }),
            ..fresh.clone()
        })
        .expect("mutate");
    let closed = Arc::new(Mutex::new(Vec::new()));
    let probe = Arc::new(FakeProbe {
        liveness: Mutex::new(liveness),
    });
    let closer: HostSessionCloser = {
        let closed = Arc::clone(&closed);
        let probe = Arc::clone(&probe);
        Arc::new(move |request: &HostSessionCloseRequest| {
            closed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(request.host_session.session_path.clone());
            if close_confirms {
                // A confirmed close ends the session on the host: later probes read it gone.
                *probe
                    .liveness
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = HostSessionLiveness::Gone;
                Ok(())
            } else {
                Err(HostSessionCloseError {
                    message: "refused".to_string(),
                })
            }
        })
    };
    let probe_for_deps: Arc<dyn HostSessionProbe> = probe;
    let mut deps = LifecycleDeps::new(
        temp.store.clone(),
        Arc::new(FakeRegistry::default()),
        default_settings(),
    );
    deps.host_session_probe = Some(probe_for_deps);
    deps.host_session_close = Some(closer);
    Fixture {
        temp,
        context: resolve_context(deps),
        closed,
    }
}

fn closed_sessions(fixture: &Fixture) -> Vec<String> {
    fixture
        .closed
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn finish(fixture: &Fixture, task_id: &str) -> Option<crate::lifecycle::ReconcileOutcome> {
    let record = fixture
        .temp
        .store
        .load(task_id)
        .expect("load")
        .expect("record");
    finish_pending_cancel(&fixture.context, &record).expect("finish")
}

#[test]
fn pending_cancel_without_a_host_session_cancels_the_record_with_its_reason() {
    let f = fixture(
        "st_0000002a",
        false,
        HostSessionLiveness::Gone,
        true,
        Some("no longer needed"),
    );
    let outcome = finish(&f, "st_0000002a").expect("finished");
    assert_eq!(outcome.kind, ReconcileOutcomeKind::Resumed);
    assert_eq!(outcome.reason.as_deref(), Some("pending cancel finished"));
    let record = f
        .temp
        .store
        .load("st_0000002a")
        .expect("load")
        .expect("record");
    assert_eq!(record.status, Cancelled);
    assert_eq!(record.error_message.as_deref(), Some("no longer needed"));
    assert_eq!(record.residency_state, Disposed);
    assert!(read_events(&f.temp.store, "st_0000002a").contains(&"cancelled".to_string()));
    assert!(closed_sessions(&f).is_empty());
}

#[test]
fn pending_cancel_without_a_reason_cancels_without_an_error_message() {
    let f = fixture("st_0000002b", false, HostSessionLiveness::Gone, true, None);
    assert!(finish(&f, "st_0000002b").is_some());
    let record = f
        .temp
        .store
        .load("st_0000002b")
        .expect("load")
        .expect("record");
    assert_eq!(record.status, Cancelled);
    assert_eq!(record.error_message, None);
}

#[test]
fn unknown_session_liveness_leaves_the_cancel_pending() {
    let f = fixture(
        "st_0000002c",
        true,
        HostSessionLiveness::Unknown,
        true,
        None,
    );
    assert_eq!(finish(&f, "st_0000002c"), None);
    let record = f
        .temp
        .store
        .load("st_0000002c")
        .expect("load")
        .expect("record");
    assert_eq!(record.status, Running);
    assert!(record.cancel_requested.is_some());
    assert!(closed_sessions(&f).is_empty());
}

#[test]
fn live_session_that_refuses_to_close_leaves_the_cancel_pending() {
    let f = fixture("st_0000002d", true, HostSessionLiveness::Live, false, None);
    assert_eq!(finish(&f, "st_0000002d"), None);
    let record = f
        .temp
        .store
        .load("st_0000002d")
        .expect("load")
        .expect("record");
    assert_eq!(record.status, Running);
    assert!(record.cancel_requested.is_some());
    assert_eq!(
        closed_sessions(&f),
        vec!["/tmp/pending-cancel.jsonl".to_string()]
    );
}

#[test]
fn live_session_is_closed_first_then_the_record_is_cancelled() {
    let f = fixture(
        "st_0000002e",
        true,
        HostSessionLiveness::Live,
        true,
        Some("stop"),
    );
    let outcome = finish(&f, "st_0000002e").expect("finished");
    assert_eq!(outcome.reason.as_deref(), Some("pending cancel finished"));
    assert_eq!(
        closed_sessions(&f),
        vec!["/tmp/pending-cancel.jsonl".to_string()]
    );
    let record = f
        .temp
        .store
        .load("st_0000002e")
        .expect("load")
        .expect("record");
    assert_eq!(record.status, Cancelled);
    let events = read_events(&f.temp.store, "st_0000002e");
    let closed_at = events
        .iter()
        .position(|event| event == "host_session_closed");
    let cancelled_at = events.iter().position(|event| event == "cancelled");
    assert!(
        closed_at.is_some() && closed_at < cancelled_at,
        "{events:?}"
    );
}

#[test]
fn gone_session_is_cancelled_without_a_close() {
    let f = fixture("st_0000002f", true, HostSessionLiveness::Gone, false, None);
    assert!(finish(&f, "st_0000002f").is_some());
    assert!(closed_sessions(&f).is_empty());
    let record = f
        .temp
        .store
        .load("st_0000002f")
        .expect("load")
        .expect("record");
    assert_eq!(record.status, Cancelled);
}
