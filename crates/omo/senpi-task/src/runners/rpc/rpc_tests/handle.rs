//! `runners/rpc/handle.test.ts` and `runners/rpc/handle-user-abort.test.ts`: the handle over a
//! fake client that acks every command and lets the test emit wire events.

use std::io;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::manager::child_handle::{ManagedChildHandle, Unsubscribe};
use crate::runners::rpc::exit_mapping::ChildExitInput;
use crate::runners::rpc::handle::{RpcChildHandle, RpcChildHandleOptions};
use crate::runners::rpc::protocol_client::{
    RpcClientError, RpcClientPort, RpcEventListener, RpcExitListener,
};
use crate::runners::types::{RpcTerminalAssistantMessage, TerminateOptions};
use crate::runners::{RunnerFailureKind, RunnerOutcome};

#[derive(Default)]
struct FakeClient {
    listeners: Mutex<Vec<RpcEventListener>>,
    exit_listeners: Mutex<Vec<RpcExitListener>>,
}

impl FakeClient {
    fn emit(&self, event: Value) {
        let listeners: Vec<RpcEventListener> = self.listeners.lock().expect("listeners").clone();
        for listener in listeners {
            listener(&event);
        }
    }

    fn exit(&self, exit: &ChildExitInput) {
        let listeners: Vec<RpcExitListener> =
            self.exit_listeners.lock().expect("exit listeners").clone();
        for listener in listeners {
            listener(exit);
        }
    }
}

impl RpcClientPort for FakeClient {
    fn pid(&self) -> Option<u32> {
        Some(4242)
    }

    fn send(&self, _command: Value) -> Result<Value, RpcClientError> {
        Ok(json!({ "success": true }))
    }

    fn on_event(&self, listener: RpcEventListener) -> Unsubscribe {
        self.listeners.lock().expect("listeners").push(listener);
        Box::new(|| {})
    }

    fn on_exit(&self, listener: RpcExitListener) -> Unsubscribe {
        self.exit_listeners
            .lock()
            .expect("exit listeners")
            .push(listener);
        Box::new(|| {})
    }

    fn stderr_tail(&self) -> String {
        String::new()
    }

    fn detach(&self) {
        self.listeners.lock().expect("listeners").clear();
    }

    fn terminate(&self, _options: TerminateOptions) -> io::Result<()> {
        Ok(())
    }
}

fn harness(task_id: &str) -> (Arc<RpcChildHandle>, Arc<FakeClient>) {
    let client = Arc::new(FakeClient::default());
    let handle = RpcChildHandle::new(RpcChildHandleOptions {
        client: Arc::clone(&client) as Arc<dyn RpcClientPort>,
        task_id: task_id.to_string(),
        heartbeat_interval_ms: 60_000,
        now: Arc::new(|| 1),
    });
    (handle, client)
}

fn agent_end() -> Value {
    json!({ "type": "agent_end", "willRetry": false, "messages": [] })
}

#[test]
fn given_an_assistant_provider_error_followed_by_terminal_agent_end_when_idle_settles_then_text_and_failure_fields_are_retained()
 {
    let (handle, client) = harness("st_00000001");
    client.emit(json!({
        "type": "message_end",
        "message": {
            "role": "assistant",
            "content": [{ "type": "text", "text": "partial" }],
            "stopReason": "error",
            "errorMessage": "401 unauthorized; re-authenticate"
        }
    }));
    client.emit(json!({ "type": "agent_end", "willRetry": false }));
    handle.wait_for_idle();
    assert_eq!(
        handle.terminal_assistant_message(),
        Some(RpcTerminalAssistantMessage {
            text: Some("partial".to_string()),
            stop_reason: Some("error".to_string()),
            error_message: Some("401 unauthorized; re-authenticate".to_string()),
        })
    );
    assert_eq!(handle.last_assistant_text().as_deref(), Some("partial"));
    assert!(!handle.was_aborted_by_user());
    handle.dispose().expect("dispose");
}

#[test]
fn given_an_explicit_abort_command_when_the_handle_records_it_then_manager_classification_can_distinguish_user_cancellation()
 {
    let (handle, _client) = harness("st_00000001");
    handle.abort().expect("abort");
    assert!(handle.was_aborted_by_user());
    handle.dispose().expect("dispose");
}

#[test]
fn given_a_completed_resident_turn_when_a_follow_up_revives_it_then_terminal_facts_reset_for_the_new_turn_while_last_text_remains_available()
 {
    let (handle, client) = harness("st_00000001");
    client.emit(json!({
        "type": "message_end",
        "message": { "role": "assistant", "content": [{ "type": "text", "text": "first" }], "stopReason": "stop" }
    }));
    client.emit(json!({ "type": "agent_end", "willRetry": false }));
    handle.wait_for_idle();
    handle.follow_up("second").expect("follow up");
    assert_eq!(handle.terminal_assistant_message(), None);
    assert_eq!(handle.last_assistant_text().as_deref(), Some("first"));
    assert!(!handle.was_aborted_by_user());
    handle.dispose().expect("dispose");
}

#[test]
fn given_a_running_turn_the_user_explicitly_aborts_when_the_terminating_agent_end_arrives_then_the_tracked_outcome_is_cancelled()
 {
    let (handle, client) = harness("st_abort_seam");
    handle.start_initial_prompt("work").expect("prompt");
    handle.abort().expect("abort");
    client.emit(agent_end());
    assert_eq!(handle.wait_for_outcome(), RunnerOutcome::Cancelled);
    handle.dispose().expect("dispose");
}

#[test]
fn given_a_turn_that_ends_in_a_provider_error_without_a_user_abort_when_the_terminating_agent_end_arrives_then_the_tracked_outcome_stays_a_turn_failure()
 {
    let (handle, client) = harness("st_abort_seam");
    handle.start_initial_prompt("work").expect("prompt");
    client.emit(json!({
        "type": "message_end",
        "message": { "role": "assistant", "content": [], "stopReason": "error", "errorMessage": "provider exploded" }
    }));
    client.emit(agent_end());
    match handle.wait_for_outcome() {
        RunnerOutcome::Error { failure, .. } => {
            assert_eq!(failure.kind, RunnerFailureKind::ChildTurnFailed);
        }
        other => panic!("expected a turn failure, got {other:?}"),
    }
    handle.dispose().expect("dispose");
}

#[test]
fn given_an_aborted_turn_followed_by_a_fresh_prompt_when_the_new_turn_ends_normally_then_the_stale_abort_no_longer_cancels_it()
 {
    let (handle, client) = harness("st_abort_seam");
    handle.start_initial_prompt("work").expect("prompt");
    handle.abort().expect("abort");
    client.emit(agent_end());
    assert_eq!(handle.wait_for_outcome(), RunnerOutcome::Cancelled);
    handle.follow_up("second").expect("follow up");
    client.emit(json!({
        "type": "message_end",
        "message": { "role": "assistant", "content": [{ "type": "text", "text": "done" }], "stopReason": "stop" }
    }));
    client.emit(agent_end());
    assert_eq!(handle.wait_for_outcome(), RunnerOutcome::completed("done"));
    handle.dispose().expect("dispose");
}

// manager/child-handle.test.ts (`adaptRpcHandle`): the RPC handle implements the managed seam
// directly, so the outcome contract is asserted through `ManagedChildHandle`.

fn terminal_turn(handle: &RpcChildHandle, client: &FakeClient, message: &Value) -> RunnerOutcome {
    handle.start_initial_prompt("work").expect("prompt");
    client.emit(json!({ "type": "message_end", "message": message }));
    client.emit(agent_end());
    ManagedChildHandle::wait_for_outcome(handle)
}

fn turn_failure(outcome: &RunnerOutcome) -> (RunnerFailureKind, String) {
    match outcome {
        RunnerOutcome::Error { failure, .. } => (failure.kind, failure.message.clone()),
        other => panic!("expected error outcome, got {other:?}"),
    }
}

#[test]
fn given_rpc_terminal_provider_error_on_resident_process_when_managed_then_child_turn_error_with_provider_message()
 {
    let (handle, client) = harness("st_00000002");

    let outcome = terminal_turn(
        &handle,
        &client,
        &json!({ "role": "assistant", "content": [], "stopReason": "error", "errorMessage": "401 unauthorized; re-authenticate" }),
    );

    assert_eq!(
        turn_failure(&outcome),
        (
            RunnerFailureKind::ChildTurnFailed,
            "401 unauthorized; re-authenticate".to_string()
        )
    );
}

#[test]
fn given_rpc_terminal_non_user_abort_when_managed_then_child_turn_error() {
    let (handle, client) = harness("st_00000002");

    let outcome = terminal_turn(
        &handle,
        &client,
        &json!({ "role": "assistant", "content": [], "stopReason": "aborted" }),
    );

    let (kind, message) = turn_failure(&outcome);
    assert_eq!(kind, RunnerFailureKind::ChildTurnFailed);
    assert!(message.contains("\"aborted\""), "{message}");
}

#[test]
fn given_rpc_turn_explicitly_aborted_by_user_when_managed_then_outcome_remains_cancelled() {
    let (handle, client) = harness("st_00000002");
    handle.start_initial_prompt("work").expect("prompt");
    handle.abort().expect("abort");

    client.emit(json!({
        "type": "message_end",
        "message": { "role": "assistant", "content": [], "stopReason": "aborted", "errorMessage": "operation aborted" }
    }));
    client.emit(agent_end());

    assert_eq!(handle.wait_for_outcome(), RunnerOutcome::Cancelled);
}

#[test]
fn given_rpc_clean_turn_with_no_assistant_output_when_managed_then_child_turn_error() {
    let (handle, client) = harness("st_00000002");

    let outcome = terminal_turn(
        &handle,
        &client,
        &json!({ "role": "assistant", "content": [], "stopReason": "stop" }),
    );

    let (kind, message) = turn_failure(&outcome);
    assert_eq!(kind, RunnerFailureKind::ChildTurnFailed);
    assert!(message.contains("no assistant output"), "{message}");
}

#[test]
fn given_rpc_clean_turn_with_assistant_text_when_managed_then_outcome_completed() {
    let (handle, client) = harness("st_00000002");

    let outcome = terminal_turn(
        &handle,
        &client,
        &json!({ "role": "assistant", "content": [{ "type": "text", "text": "rpc final" }], "stopReason": "stop" }),
    );

    assert_eq!(ManagedChildHandle::pid(handle.as_ref()), Some(4242));
    assert_eq!(outcome, RunnerOutcome::completed("rpc final"));
}

#[test]
fn given_rpc_handle_that_crashed_when_managed_then_error_carries_stderr_tail() {
    let (handle, client) = harness("st_00000002");
    handle.start_initial_prompt("work").expect("prompt");

    client.exit(&ChildExitInput {
        code: Some(1),
        signal: None,
        error: None,
        pid: Some(4242),
        stderr: "boom".to_string(),
    });

    let (kind, message) = turn_failure(&handle.wait_for_outcome());
    assert_eq!(kind, RunnerFailureKind::ChildPromptFailed);
    assert!(message.contains("boom"), "{message}");
}
