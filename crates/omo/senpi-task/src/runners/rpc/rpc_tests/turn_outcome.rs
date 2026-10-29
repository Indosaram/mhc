use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::runners::rpc::turn_outcome::{
    agent_end_outcome, exit_turn_outcome, extract_assistant_text,
};
use crate::runners::types::{ChildExitFacts, ChildExitOutcome};
use crate::runners::{RunnerFailureKind, RunnerOutcome};

fn malformed_agent_end(overrides: Value) -> Value {
    let mut event = json!({ "type": "agent_end", "willRetry": false });
    if let (Some(target), Some(extra)) = (event.as_object_mut(), overrides.as_object()) {
        target.extend(extra.clone());
    }
    event
}

fn failure_kind(outcome: &RunnerOutcome) -> Option<RunnerFailureKind> {
    match outcome {
        RunnerOutcome::Error { failure, .. } => Some(failure.kind),
        _ => None,
    }
}

#[test]
fn absent_messages_degrade_to_terminal_error() {
    let outcome = agent_end_outcome(&malformed_agent_end(json!({})), None, None);
    assert!(matches!(outcome, RunnerOutcome::Error { .. }));
}

#[test]
fn non_array_messages_degrade_to_terminal_error() {
    let event = malformed_agent_end(json!({ "messages": { "role": "assistant" } }));
    let outcome = agent_end_outcome(&event, None, None);
    assert!(matches!(outcome, RunnerOutcome::Error { .. }));
}

#[test]
fn null_messages_with_fresh_observed_text_complete() {
    let event = malformed_agent_end(json!({ "messages": null }));
    let outcome = agent_end_outcome(&event, Some("old"), Some("fresh"));
    assert_eq!(outcome, RunnerOutcome::completed("fresh"));
}

#[test]
fn malformed_aborted_agent_end_keeps_abort_classification() {
    let event = malformed_agent_end(json!({ "aborted": true }));
    let outcome = agent_end_outcome(&event, None, None);
    assert_eq!(
        failure_kind(&outcome),
        Some(RunnerFailureKind::ChildTurnFailed)
    );
}

#[test]
fn assistant_text_prefers_the_last_assistant_message() {
    let event = malformed_agent_end(json!({ "messages": [
        { "role": "assistant", "content": [{ "type": "text", "text": "first" }] },
        { "role": "user", "content": [{ "type": "text", "text": "ignored" }] },
        { "role": "assistant", "content": [{ "type": "text", "text": "a" }, { "type": "image" }, { "type": "text", "text": "b" }] }
    ] }));
    assert_eq!(
        agent_end_outcome(&event, None, None),
        RunnerOutcome::completed("ab")
    );
    assert_eq!(
        extract_assistant_text(&json!({ "role": "user", "content": [] })),
        None
    );
}

#[test]
fn error_stop_reason_is_a_turn_failure_with_the_error_message() {
    let event = malformed_agent_end(json!({ "messages": [
        { "role": "assistant", "content": [], "stopReason": "error", "errorMessage": "boom" }
    ] }));
    assert_eq!(
        agent_end_outcome(&event, None, None),
        RunnerOutcome::error(RunnerFailureKind::ChildTurnFailed, "boom")
    );
}

#[test]
fn clean_exit_without_text_and_killed_exit_are_errors() {
    let clean = ChildExitOutcome::Clean {
        facts: ChildExitFacts::default(),
    };
    assert_eq!(
        exit_turn_outcome(&clean, Some("done")),
        RunnerOutcome::completed("done")
    );
    assert_eq!(
        failure_kind(&exit_turn_outcome(&clean, None)),
        Some(RunnerFailureKind::ChildTurnFailed)
    );
    let killed = ChildExitOutcome::Killed {
        facts: ChildExitFacts {
            pid: Some(7),
            signal: Some("SIGKILL".to_string()),
            ..ChildExitFacts::default()
        },
    };
    let RunnerOutcome::Error { failure, killed } = exit_turn_outcome(&killed, Some("partial"))
    else {
        panic!("killed exit must be an error");
    };
    assert_eq!(failure.kind, RunnerFailureKind::ChildPromptFailed);
    assert!(killed);
    assert!(failure.message.contains("SIGKILL"), "{}", failure.message);
}
