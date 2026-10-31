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

/// `session-confirmed-close.test.ts` `closureFixture("answers", acked: true, listed: true)`: the
/// real session connection over a fake host RPC client that acknowledges `close_session`, emits a
/// `session_closed` lifecycle frame during it, and still lists the session afterwards.
struct ClosingHost {
    frame: Option<&'static str>,
    listeners: Mutex<Vec<ManagedChildListener>>,
    listings: Mutex<usize>,
    detaches: Mutex<usize>,
}

impl ClosingHost {
    fn emit(&self, reason: &str) {
        let frame = crate::shared::ManagedChildEvent {
            event_type: "session_closed".into(),
            reason: Some(reason.into()),
            ..crate::shared::ManagedChildEvent::default()
        };
        let listeners: Vec<_> = self.listeners.lock().expect("listeners").clone();
        for listener in listeners {
            listener(&frame);
        }
    }
}

impl super::super::host_command_dispatch::HostRpcClient for ClosingHost {
    fn command(&self, _command: Value) -> Result<(), HostError> {
        Ok(())
    }
    fn get_state(&self) -> Result<Value, HostError> {
        Err(HostError {
            message: "Timeout waiting for response to get_state".into(),
        })
    }
    fn on_event(&self, listener: ManagedChildListener) -> Unsubscribe {
        self.listeners.lock().expect("listeners").push(listener);
        Box::new(|| {})
    }
    fn close(&self, _routing_id: &str) -> Result<(), HostError> {
        if let Some(reason) = self.frame {
            self.emit(reason);
        }
        Ok(())
    }
    fn list_sessions(&self) -> Result<Option<Vec<String>>, HostError> {
        *self.listings.lock().expect("listings") += 1;
        Ok(Some(vec!["closing-session".to_string()]))
    }
    fn detach(&self, _routing_id: &str) -> Result<(), HostError> {
        *self.detaches.lock().expect("detaches") += 1;
        Ok(())
    }
}

impl super::super::task_parent_wire::HostRunHoldClient for ClosingHost {}

fn closing(
    frame: Option<&'static str>,
) -> (
    Arc<HostSessionHandle>,
    Arc<ClosingHost>,
    Arc<Mutex<Vec<&'static str>>>,
) {
    let host = Arc::new(ClosingHost {
        frame,
        listeners: Mutex::new(Vec::new()),
        listings: Mutex::new(0),
        detaches: Mutex::new(0),
    });
    let connection = super::super::session_client_connection::SessionClientConnection::new(
        "closing-session".into(),
        Arc::clone(&host) as Arc<dyn super::super::host_command_dispatch::HostRpcClient>,
        Arc::clone(&host) as Arc<dyn super::super::task_parent_wire::HostRunHoldClient>,
    );
    let opened = OpenedHostSession {
        session: HostSessionIdentity {
            routing_id: "closing-session".into(),
            session_path: "/tmp/sessions/close-proof.jsonl".into(),
            instance_id: "close-proof".into(),
        },
        attached: true,
        durable_session_id: None,
        reported_model: None,
    };
    let handle = create_host_session_handle(
        "st_close_proof".into(),
        Arc::new(connection) as Arc<dyn HostSessionPort>,
        opened,
        Arc::new(|| RunnerOutcome::Cancelled),
    );
    let notices = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&notices);
    let _kept = ManagedChildHandle::on_liveness(
        handle.as_ref(),
        Arc::new(move |status| {
            sink.lock().expect("notices").push(match status {
                HostLivenessStatus::Running => "running",
                HostLivenessStatus::Unresponsive { .. } => "unresponsive",
                HostLivenessStatus::TerminalPending { .. } => "terminal-pending",
            });
        }),
    );
    (handle, host, notices)
}

/// `session-confirmed-close.test.ts` "a {handoff_parked, client_close} frame cannot confirm an
/// overdue session that remains listed": the frame arrives during the confirmed close and is a
/// hint, not proof; the still-listed session stays owned and terminal-pending.
#[test]
fn a_lifecycle_frame_cannot_confirm_an_overdue_session_that_remains_listed() {
    for reason in ["handoff_parked", "client_close"] {
        let (handle, host, notices) = closing(Some(reason));
        let start = now_ms() + HOST_HEARTBEAT_INTERVAL_MS;
        miss_three(&handle, start);
        // When the liveness deadline requests closure.
        handle
            .liveness_step_at(start + 170_000)
            .expect("terminal-pending step");
        handle.settle_close_for_tests();
        assert!(!handle.stopped(), "{reason}: the session has not exited");
        assert!(
            matches!(
                handle.liveness(),
                Some(HostLivenessStatus::TerminalPending { .. })
            ),
            "{reason}"
        );
        assert_eq!(*host.listings.lock().expect("listings"), 1, "{reason}");
        assert_eq!(
            *notices.lock().expect("notices"),
            vec!["unresponsive", "terminal-pending"],
            "{reason}"
        );
    }
}

/// `handle.ts` `clientBinding.onClosed`: outside the confirmed close, a `session_closed` frame
/// ends this child's session (`endSession`), and is never counted as liveness activity.
#[test]
fn a_session_closed_frame_on_a_running_session_ends_it() {
    let (handle, host, notices) = closing(None);
    host.emit("client_close");
    assert!(handle.stopped());
    assert!(notices.lock().expect("notices").is_empty());
    assert_eq!(*host.listings.lock().expect("listings"), 0);
    assert_eq!(*host.detaches.lock().expect("detaches"), 1);
}
