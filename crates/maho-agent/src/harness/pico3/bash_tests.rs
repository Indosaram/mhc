use std::cell::RefCell;
use super::*;
use crate::harness::context::{background_context, with_abort_signal};
use maho_ai::utils::abort::AbortController;

fn details(result: &ToolResult) -> BashDetails { Stored::from_json(result.details.clone().expect("details")).decode().expect("bash details") }

#[tokio::test]
async fn per_call_cwd_streams_both_pipes_and_preserves_nonzero_exit() {
    let directory = tempfile::tempdir().expect("directory");
    let output = RefCell::new(Vec::new());
    let stream = |chunk: &[u8]| { output.borrow_mut().extend_from_slice(chunk); Ok(()) };
    let pwd = if cfg!(windows) { "pwd -W" } else { "pwd" };
    let args = BashArgs { command: format!("{pwd}; printf 'stderr-marker' >&2; exit 7"), cwd: Some(directory.path().to_string_lossy().into_owned()) };
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), execute(args, &stream, &background_context())).await.expect("bounded child completion").expect("execution");
    assert_eq!(result.is_error, Some(true));
    assert_eq!(details(&result).exit_code, Some(7));
    let output = String::from_utf8(output.into_inner()).expect("UTF-8");
    let expected = directory.path().to_str().expect("directory path").replace('\\', "/");
    assert!(output.replace('\\', "/").contains(&expected));
    assert!(output.contains("stderr-marker"));
}

#[tokio::test]
async fn clean_exit_has_zero_code_and_no_signal() {
    let result = execute(BashArgs { command: "exit 0".into(), cwd: None }, &|_| Ok(()), &background_context()).await.expect("execute");
    assert_eq!(result.is_error, Some(false));
    assert_eq!(details(&result).exit_code, Some(0));
    assert_eq!(details(&result).signal, None);
}

#[cfg(unix)]
#[tokio::test]
async fn signal_exit_reports_signal_and_null_exit_code() {
    let result = execute(BashArgs { command: "kill -TERM $$".into(), cwd: None }, &|_| Ok(()), &background_context()).await.expect("execute");
    assert_eq!(result.is_error, Some(true));
    assert_eq!(details(&result).exit_code, None);
    assert_eq!(details(&result).signal.as_deref(), Some("SIGTERM"));
}

#[tokio::test]
async fn abort_after_output_kills_and_reaps_child_without_timer_race() {
    let controller = AbortController::new();
    let ctx = with_abort_signal(controller.signal(), &background_context());
    let stage = std::cell::Cell::new("waiting for first output to trigger abort");
    let stream = |_: &[u8]| { stage.set("output received; waiting for abort cleanup and child reap"); controller.abort(None); Ok(()) };
    let command = if cfg!(windows) { "bash -c 'printf ready; while :; do :; done' & wait" } else { "printf ready; while :; do :; done" };
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), execute(BashArgs { command: command.into(), cwd: None }, &stream, &ctx)).await.unwrap_or_else(|_| panic!("pico3 abort execution did not finish within 5s: {}", stage.get()));
    assert!(matches!(result, Err(Error::Forbidden(_))));
    eprintln!("pico3 abort execution and assertions finished; runtime shutdown is next");
}

#[tokio::test]
async fn stream_failure_is_propagated_and_not_a_success_result() {
    let stage = std::cell::Cell::new("waiting for first output to trigger sink failure");
    let stream = |_: &[u8]| { stage.set("sink failed; waiting for error cleanup and child reap"); Err(Error::Faulted("sink failed".into())) };
    let command = if cfg!(windows) { "bash -c 'printf output; while :; do :; done' & wait" } else { "printf output; while :; do :; done" };
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), execute(BashArgs { command: command.into(), cwd: None }, &stream, &background_context())).await.unwrap_or_else(|_| panic!("pico3 sink-failure execution did not finish within 5s: {}", stage.get()));
    assert!(matches!(result, Err(Error::Faulted(_))));
    eprintln!("pico3 sink-failure execution and assertions finished; runtime shutdown is next");
}

#[test]
fn declaration_is_replay_unsafe_with_supplied_bounds() {
    let declaration = bash_tool(OutputLimits { max_bytes: Some(13), max_lines: Some(2), retain: None }).expect("declaration");
    assert!(matches!(ToolDeclaration::replay(&declaration), Replay::Unsafe));
    assert_eq!(ToolDeclaration::output(&declaration).max_bytes, Some(13));
    assert!(declaration.parameters.get("required").is_some());
}

