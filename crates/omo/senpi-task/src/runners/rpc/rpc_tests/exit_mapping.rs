//! `runners/rpc/exit-mapping.test.ts`.

use crate::runners::rpc::exit_mapping::{
    ChildExitInput, classify_child_exit, map_exit_outcome_to_error, tail_stderr,
};
use crate::runners::types::ChildExitOutcome;

fn input(
    code: Option<i32>,
    signal: Option<&str>,
    pid: Option<i64>,
    stderr: &str,
) -> ChildExitInput {
    ChildExitInput {
        code,
        signal: signal.map(str::to_string),
        error: None,
        pid,
        stderr: stderr.to_string(),
    }
}

#[test]
fn spawn_error_is_a_spawn_error_outcome() {
    let outcome = classify_child_exit(&ChildExitInput {
        error: Some("ENOENT".to_string()),
        ..ChildExitInput::default()
    });
    let ChildExitOutcome::SpawnError { message, .. } = outcome else {
        panic!("expected spawn_error, got {outcome:?}");
    };
    assert!(message.contains("ENOENT"));
}

#[test]
fn exit_by_signal_is_killed_with_the_signal_recorded() {
    let outcome = classify_child_exit(&input(None, Some("SIGKILL"), Some(4321), ""));
    assert_eq!(outcome.kind(), "killed");
    assert_eq!(outcome.facts().signal.as_deref(), Some("SIGKILL"));
    assert_eq!(outcome.facts().pid, Some(4321));
}

#[test]
fn zero_exit_code_is_clean() {
    assert_eq!(
        classify_child_exit(&input(Some(0), None, Some(1), "")).kind(),
        "clean"
    );
}

#[test]
fn nonzero_exit_is_crashed_and_stderr_tail_is_capped() {
    let outcome = classify_child_exit(&input(Some(3), None, Some(9), &"x".repeat(5000)));
    assert_eq!(outcome.kind(), "crashed");
    assert_eq!(outcome.facts().code, Some(3));
    assert_eq!(outcome.facts().stderr_tail.len(), 4096);
}

#[test]
fn tail_keeps_the_last_cap_characters() {
    assert_eq!(tail_stderr("abcdef", 4), "cdef");
    assert_eq!(tail_stderr("ab", 4), "ab");
}

#[test]
fn killed_before_terminal_maps_to_error_with_killed_and_exit_facts() {
    let outcome = classify_child_exit(&input(None, Some("SIGKILL"), Some(77), ""));
    let mapped = map_exit_outcome_to_error(&outcome, false).expect("mapped");
    assert!(mapped.killed);
    assert_eq!(mapped.exit.signal.as_deref(), Some("SIGKILL"));
    assert_eq!(mapped.exit.pid, Some(77));
    assert!(mapped.error_message.contains("SIGKILL"));
}

#[test]
fn nonzero_exit_before_terminal_carries_the_stderr_tail() {
    let outcome = classify_child_exit(&input(Some(2), None, Some(5), "boom failure"));
    let mapped = map_exit_outcome_to_error(&outcome, false).expect("mapped");
    assert!(!mapped.killed);
    assert!(mapped.error_message.contains("boom failure"));
}

#[test]
fn exit_after_terminal_is_resident_teardown() {
    let outcome = classify_child_exit(&input(Some(0), None, Some(5), ""));
    assert_eq!(map_exit_outcome_to_error(&outcome, true), None);
}
