//! `child-handle-outcome.test.ts`, `child-handle-restored.test.ts`, `child-handle-revive.test.ts`.

use serde_json::{Value, json};

use super::fake_session::FakeSession;
use crate::manager::child_handle::ManagedChildHandle;
use crate::runners::in_process::child_handle::{
    ChildCompletionPolicy, InProcessChildHandle, RunnerFailureKind, RunnerOutcome,
};

fn assistant_end(text: &str, stop_reason: &str, error_message: Option<&str>) -> Value {
    let content = if text.is_empty() {
        json!([])
    } else {
        json!([{ "type": "text", "text": text }])
    };
    let mut message = json!({ "role": "assistant", "content": content, "stopReason": stop_reason });
    if let Some(error_message) = error_message {
        message["errorMessage"] = json!(error_message);
    }
    json!({ "type": "message_end", "message": message })
}

fn failure_message(outcome: &RunnerOutcome) -> &str {
    match outcome {
        RunnerOutcome::Error { failure, .. } => &failure.message,
        other => panic!("expected error outcome, got {other:?}"),
    }
}

#[test]
fn given_a_turn_ending_with_a_stop_reason_error_message_when_the_prompt_resolves_then_the_outcome_is_an_error_carrying_the_provider_message()
 {
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::start("task-1", fake.clone(), "review this");
    fake.wait_prompt_calls(1);
    fake.emit(&assistant_end(
        "",
        "error",
        Some("upstream gateway timeout"),
    ));
    fake.resolve_prompt();
    assert!(failure_message(&handle.wait_for_idle()).contains("upstream gateway timeout"));
}

#[test]
fn given_a_turn_that_produces_no_assistant_output_at_all_when_the_prompt_resolves_then_the_outcome_is_an_error_never_completed_with_empty_text()
 {
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::start("task-1", fake.clone(), "review this");
    fake.resolve_prompt();
    assert!(failure_message(&handle.wait_for_idle()).contains("no assistant output"));
}

#[test]
fn given_a_revived_child_whose_new_turn_produces_nothing_when_the_revive_turn_resolves_then_the_stale_previous_response_is_not_reused_as_a_fresh_completion()
 {
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::start("task-1", fake.clone(), "first run");
    fake.wait_prompt_calls(1);
    fake.emit(&assistant_end("first verdict", "stop", None));
    fake.set_last_text("first verdict");
    fake.resolve_prompt();
    assert_eq!(
        handle.wait_for_idle(),
        RunnerOutcome::completed("first verdict")
    );
    handle
        .follow_up_turn("re-emit your verdict")
        .expect("follow up");
    fake.resolve_prompt();
    assert!(matches!(
        handle.wait_for_idle(),
        RunnerOutcome::Error { .. }
    ));
}

#[test]
fn given_a_healthy_turn_with_assistant_text_when_the_prompt_resolves_then_the_outcome_completes_with_this_turns_text()
 {
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::start("task-1", fake.clone(), "do the work");
    fake.wait_prompt_calls(1);
    fake.emit(&assistant_end("all done", "stop", None));
    fake.resolve_prompt();
    assert_eq!(handle.wait_for_idle(), RunnerOutcome::completed("all done"));
}

#[test]
fn given_a_restored_session_with_prior_assistant_output_when_the_handle_is_created_then_no_prompt_is_replayed_and_wait_for_idle_drains_the_transcript_outcome()
 {
    let fake = FakeSession::new("child-session-1");
    fake.set_last_text("previous result");
    let handle = InProcessChildHandle::restored("task-1", fake.clone());
    assert_eq!(fake.prompt_calls(), 0);
    assert_eq!(
        handle.wait_for_idle(),
        RunnerOutcome::completed("previous result")
    );
}

#[test]
fn given_a_restored_session_without_assistant_output_when_wait_for_idle_is_awaited_then_a_typed_empty_turn_outcome_settles_instead_of_hanging()
 {
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::restored("task-1", fake.clone());
    match handle.wait_for_idle() {
        RunnerOutcome::Error { failure, .. } => {
            assert_eq!(failure.kind, RunnerFailureKind::ChildTurnFailed);
        }
        other => panic!("expected error, got {other:?}"),
    }
    assert_eq!(fake.prompt_calls(), 0);
}

#[test]
fn given_an_idle_restored_handle_when_a_follow_up_arrives_then_a_fresh_tracked_turn_starts_from_the_follow_up_text()
 {
    let fake = FakeSession::new("child-session-1");
    fake.set_last_text("previous result");
    let handle = InProcessChildHandle::restored("task-1", fake.clone());
    handle
        .follow_up_turn("continue the work")
        .expect("follow up");
    fake.wait_prompt_calls(1);
    assert!(fake.follow_up_calls().is_empty());
    fake.set_last_text("new result");
    fake.resolve_prompt();
    assert_eq!(
        handle.wait_for_idle(),
        RunnerOutcome::completed("new result")
    );
}

#[test]
fn given_a_restored_handle_with_a_turn_in_flight_when_a_follow_up_arrives_then_it_queues_on_the_session_exactly_like_a_live_resident()
 {
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::restored("task-1", fake.clone());
    handle.follow_up_turn("first").expect("first");
    handle.follow_up_turn("second").expect("second");
    fake.wait_prompt_calls(1);
    assert_eq!(fake.follow_up_calls(), vec!["second".to_string()]);
    fake.set_last_text("done");
    fake.resolve_prompt();
    assert_eq!(handle.wait_for_idle(), RunnerOutcome::completed("done"));
}

#[test]
fn given_a_running_child_when_followed_up_mid_turn_then_it_queues_via_follow_up_without_starting_a_new_turn()
 {
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::start("task-1", fake.clone(), "do the work");
    handle.follow_up_turn("more context").expect("follow up");
    assert_eq!(fake.follow_up_calls(), vec!["more context".to_string()]);
    fake.wait_prompt_calls(1);
    fake.set_last_text("done");
    fake.resolve_prompt();
    assert_eq!(handle.wait_for_idle(), RunnerOutcome::completed("done"));
}

#[test]
fn given_a_completed_resident_child_when_revived_with_a_follow_up_then_a_fresh_turn_is_tracked_and_wait_for_idle_reflects_the_new_outcome()
 {
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::start("task-1", fake.clone(), "do the work");
    fake.set_last_text("first");
    fake.resolve_prompt();
    assert_eq!(handle.wait_for_idle(), RunnerOutcome::completed("first"));
    assert_eq!(fake.prompt_calls(), 1);
    handle.follow_up_turn("again").expect("follow up");
    fake.set_last_text("second");
    fake.wait_prompt_calls(2);
    fake.resolve_prompt();
    assert_eq!(handle.wait_for_idle(), RunnerOutcome::completed("second"));
}

// manager/child-handle.test.ts (`adaptInProcessHandle`): the in-process handle implements the
// managed seam directly.

#[test]
fn given_in_process_handle_when_managed_then_pid_absent_and_outcome_forwarded() {
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::start("st_00000001", fake.clone(), "work");
    fake.wait_prompt_calls(1);
    fake.emit(&assistant_end("done", "stop", None));
    fake.resolve_prompt();

    let outcome = ManagedChildHandle::wait_for_outcome(handle.as_ref());

    assert_eq!(ManagedChildHandle::pid(handle.as_ref()), None);
    assert_eq!(
        ManagedChildHandle::session_id(handle.as_ref()).as_deref(),
        Some("child-session-1")
    );
    assert_eq!(outcome, RunnerOutcome::completed("done"));
}

#[test]
fn given_in_process_handle_when_disposed_via_seam_then_dispose_succeeds_and_outcome_stays() {
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::start("st_00000001", fake.clone(), "work");
    fake.wait_prompt_calls(1);
    handle.abort_turn().expect("abort");
    fake.resolve_prompt();
    assert_eq!(handle.wait_for_idle(), RunnerOutcome::Cancelled);

    ManagedChildHandle::dispose(handle.as_ref()).expect("dispose");

    assert_eq!(
        ManagedChildHandle::wait_for_outcome(handle.as_ref()),
        RunnerOutcome::Cancelled
    );
    assert_eq!(fake.disposed(), 1);
}

#[test]
fn given_an_explicit_final_text_policy_and_a_textless_turn_when_it_settles_then_the_outcome_is_a_child_turn_failed_error()
{
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::start_with_completion(
        "task-1",
        fake.clone(),
        "review this",
        ChildCompletionPolicy::FinalText,
    );
    fake.wait_prompt_calls(1);
    fake.emit(&assistant_end("", "stop", None));
    fake.resolve_prompt();
    match handle.wait_for_idle() {
        RunnerOutcome::Error { failure, .. } => {
            assert_eq!(failure.kind, RunnerFailureKind::ChildTurnFailed);
        }
        other => panic!("expected error, got {other:?}"),
    }
}

#[test]
fn given_the_turn_policy_and_a_textless_normally_settled_turn_when_it_settles_then_the_outcome_is_a_completed_empty_turn()
{
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::start_with_completion(
        "task-1",
        fake.clone(),
        "run the tool",
        ChildCompletionPolicy::Turn,
    );
    fake.wait_prompt_calls(1);
    fake.emit(&assistant_end("", "stop", None));
    fake.resolve_prompt();
    assert_eq!(handle.wait_for_idle(), RunnerOutcome::completed(""));
}

#[test]
fn given_the_turn_policy_when_a_restored_handle_with_no_transcript_text_drains_then_the_outcome_is_a_completed_empty_turn()
{
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::restored_with_completion(
        "task-1",
        fake.clone(),
        ChildCompletionPolicy::Turn,
    );
    assert_eq!(fake.prompt_calls(), 0);
    assert_eq!(handle.wait_for_idle(), RunnerOutcome::completed(""));
}

#[test]
fn given_the_turn_policy_and_a_live_turn_that_emits_a_stop_reason_error_when_it_settles_then_the_failure_still_wins()
{
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::start_with_completion(
        "task-1",
        fake.clone(),
        "run the tool",
        ChildCompletionPolicy::Turn,
    );
    fake.wait_prompt_calls(1);
    fake.emit(&assistant_end("", "error", Some("provider exploded")));
    fake.resolve_prompt();
    match handle.wait_for_idle() {
        RunnerOutcome::Error { failure, .. } => {
            assert_eq!(failure.kind, RunnerFailureKind::ChildTurnFailed);
            assert!(failure.message.contains("provider exploded"));
        }
        other => panic!("expected error, got {other:?}"),
    }
}

#[test]
fn given_the_turn_policy_and_a_live_turn_that_emits_a_stop_reason_aborted_when_it_settles_then_the_failure_still_wins()
{
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::start_with_completion(
        "task-1",
        fake.clone(),
        "run the tool",
        ChildCompletionPolicy::Turn,
    );
    fake.wait_prompt_calls(1);
    fake.emit(&assistant_end("", "aborted", None));
    fake.resolve_prompt();
    match handle.wait_for_idle() {
        RunnerOutcome::Error { failure, .. } => {
            assert_eq!(failure.kind, RunnerFailureKind::ChildTurnFailed);
        }
        other => panic!("expected error, got {other:?}"),
    }
}

#[test]
fn given_the_turn_policy_and_a_prompt_that_rejects_when_the_turn_settles_then_the_outcome_is_a_child_prompt_failed_error()
{
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::start_with_completion(
        "task-1",
        fake.clone(),
        "run the tool",
        ChildCompletionPolicy::Turn,
    );
    fake.wait_prompt_calls(1);
    fake.reject_prompt("prompt transport failed");
    match handle.wait_for_idle() {
        RunnerOutcome::Error { failure, .. } => {
            assert_eq!(failure.kind, RunnerFailureKind::ChildPromptFailed);
            assert!(failure.message.contains("prompt transport failed"));
        }
        other => panic!("expected error, got {other:?}"),
    }
}

#[test]
fn given_the_turn_policy_and_an_aborted_turn_when_the_prompt_resolves_then_cancellation_wins_over_the_turn_policy()
{
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::start_with_completion(
        "task-1",
        fake.clone(),
        "run the tool",
        ChildCompletionPolicy::Turn,
    );
    fake.wait_prompt_calls(1);
    handle.abort_turn().expect("abort");
    fake.resolve_prompt();
    assert_eq!(handle.wait_for_idle(), RunnerOutcome::Cancelled);
}

#[test]
fn given_the_turn_policy_when_an_idle_child_is_revived_with_a_follow_up_then_the_new_textless_turn_is_also_completed_empty()
{
    let fake = FakeSession::new("child-session-1");
    let handle = InProcessChildHandle::start_with_completion(
        "task-1",
        fake.clone(),
        "run the tool",
        ChildCompletionPolicy::Turn,
    );
    fake.wait_prompt_calls(1);
    fake.emit(&assistant_end("", "stop", None));
    fake.resolve_prompt();
    assert_eq!(handle.wait_for_idle(), RunnerOutcome::completed(""));
    handle.follow_up_turn("run it again").expect("follow up");
    fake.wait_prompt_calls(2);
    fake.emit(&assistant_end("", "stop", None));
    fake.resolve_prompt();
    assert_eq!(handle.wait_for_idle(), RunnerOutcome::completed(""));
}
