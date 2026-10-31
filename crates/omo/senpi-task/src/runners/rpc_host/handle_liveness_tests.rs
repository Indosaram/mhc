//! Ports `rpc-host/handle-liveness.test.ts` against an explicit clock.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::*;
use crate::state::host_liveness::HostLivenessStatus;

#[derive(Default)]
struct StalledPort {
    sent: Mutex<Vec<Value>>,
    /// `None`: every state probe is refused (stalled); `Some`: it answers.
    state: Mutex<Option<Value>>,
    missing: Mutex<bool>,
    closes: Mutex<usize>,
    detaches: Mutex<usize>,
    lost: Mutex<Vec<String>>,
}

impl HostSessionPort for StalledPort {
    fn send(&self, command: Value) -> Result<(), HostError> {
        self.sent.lock().expect("sent").push(command);
        Ok(())
    }
    fn get_state(&self) -> Result<Value, HostError> {
        if *self.missing.lock().expect("missing") {
            return Err(HostError {
                message: "unknown_session".into(),
            });
        }
        self.state.lock().expect("state").clone().ok_or(HostError {
            message: "Timeout waiting for response to get_state".into(),
        })
    }
    fn on_event(&self, _listener: ManagedChildListener) -> Unsubscribe {
        Box::new(|| {})
    }
    fn close(&self) -> Result<(), HostError> {
        Ok(())
    }
    fn detach(&self) -> Result<(), HostError> {
        *self.detaches.lock().expect("detaches") += 1;
        Ok(())
    }
    fn transport_lost(&self, message: String) {
        self.lost.lock().expect("lost").push(message);
    }
    fn wait_for_outcome(&self) -> Option<RunnerOutcome> {
        Some(RunnerOutcome::Cancelled)
    }
    fn close_confirmed(&self) -> Option<Result<(), HostError>> {
        *self.closes.lock().expect("closes") += 1;
        Some(Ok(()))
    }
}

fn stalled() -> (Arc<HostSessionHandle>, Arc<StalledPort>) {
    let port = Arc::new(StalledPort::default());
    let opened = OpenedHostSession {
        session: HostSessionIdentity {
            routing_id: "route".into(),
            session_path: "path".into(),
            instance_id: "host".into(),
        },
        attached: true,
        durable_session_id: None,
        reported_model: None,
    };
    let handle = create_host_session_handle(
        "st_00000001".into(),
        Arc::clone(&port) as Arc<dyn HostSessionPort>,
        opened,
        Arc::new(|| RunnerOutcome::Cancelled),
    );
    (handle, port)
}

/// Three refused probes, starting `at`: the session publishes `unresponsive`.
fn miss_three(handle: &HostSessionHandle, at: i64) {
    for step in 0..3 {
        let _ = handle.liveness_step_at(at + step * HOST_HEARTBEAT_INTERVAL_MS);
    }
}

#[test]
fn missed_probes_keep_the_child_nonterminal_and_refuse_input_until_it_answers() {
    let (handle, port) = stalled();
    let start = now_ms() + HOST_HEARTBEAT_INTERVAL_MS;
    miss_three(&handle, start);
    assert!(matches!(
        handle.liveness(),
        Some(HostLivenessStatus::Unresponsive { .. })
    ));
    let refused = handle.steer("queued").expect_err("nothing is sent");
    assert!(crate::runners::rpc_host::session_wire::is_host_session_unresponsive(&refused));
    assert!(port.sent.lock().expect("sent").is_empty());
    assert_eq!(*port.detaches.lock().expect("detaches"), 0);
    *port.state.lock().expect("state") = Some(json!({ "sessionId": "same-child" }));
    handle
        .liveness_step_at(start + 60_000)
        .expect("a good probe");
    assert_eq!(handle.liveness(), Some(HostLivenessStatus::Running));
    handle.steer("continue").expect("steer");
    let steered: Vec<Value> = port.sent.lock().expect("sent").clone();
    assert_eq!(steered[0]["type"], "steer");
    assert_eq!(steered[0]["message"], "continue");
}

#[test]
fn an_overdue_session_is_aborted_and_closed_before_reporting_terminal() {
    let (handle, port) = stalled();
    let start = now_ms() + HOST_HEARTBEAT_INTERVAL_MS;
    miss_three(&handle, start);
    handle
        .liveness_step_at(start + 170_000)
        .expect("terminal-pending step");
    assert!(matches!(
        handle.liveness(),
        Some(HostLivenessStatus::TerminalPending { .. })
    ));
    handle.settle_close_for_tests();
    assert!(
        port.sent
            .lock()
            .expect("sent")
            .iter()
            .any(|command| command["type"] == "abort")
    );
    assert_eq!(*port.closes.lock().expect("closes"), 1);
    assert!(handle.stopped());
    let RunnerOutcome::Error { failure, .. } = handle.wait_for_outcome() else {
        panic!("expected the unresponsive close outcome");
    };
    assert_eq!(
        failure.kind,
        crate::runners::RunnerFailureKind::ChildUnresponsive
    );
}

#[test]
fn a_live_hosts_missing_session_reply_ends_the_handle_immediately() {
    let (handle, port) = stalled();
    *port.missing.lock().expect("missing") = true;
    handle
        .liveness_step_at(now_ms() + HOST_HEARTBEAT_INTERVAL_MS)
        .expect("gone is not a transport loss");
    assert!(handle.stopped());
    assert_eq!(*port.closes.lock().expect("closes"), 0);
    assert_eq!(*port.detaches.lock().expect("detaches"), 1);
    assert_eq!(
        *port.lost.lock().expect("lost"),
        vec!["host session closed: unknown_session".to_string()]
    );
}
