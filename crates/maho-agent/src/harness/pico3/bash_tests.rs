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
    let args = BashArgs { command: "printf '%s\\n' \"$PWD\"; printf 'stderr-marker' >&2; exit 7".into(), cwd: Some(directory.path().to_string_lossy().into_owned()) };
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), execute(args, &stream, &background_context())).await.expect("bounded child completion").expect("execution");
    assert_eq!(result.is_error, Some(true));
    assert_eq!(details(&result).exit_code, Some(7));
    let output = String::from_utf8(output.into_inner()).expect("UTF-8");
    assert!(output.contains(directory.path().to_str().expect("directory path")));
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
    let stream = |_: &[u8]| { controller.abort(None); Ok(()) };
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), execute(BashArgs { command: "printf ready; while :; do :; done".into(), cwd: None }, &stream, &ctx)).await.expect("abort completion");
    assert!(matches!(result, Err(Error::Forbidden(_))));
}

#[tokio::test]
async fn stream_failure_is_propagated_and_not_a_success_result() {
    let stream = |_: &[u8]| Err(Error::Faulted("sink failed".into()));
    let result = execute(BashArgs { command: "printf output; while :; do :; done".into(), cwd: None }, &stream, &background_context()).await;
    assert!(matches!(result, Err(Error::Faulted(_))));
}

#[test]
fn declaration_is_replay_unsafe_with_supplied_bounds() {
    let declaration = bash_tool(OutputLimits { max_bytes: Some(13), max_lines: Some(2), retain: None }).expect("declaration");
    assert!(matches!(ToolDeclaration::replay(&declaration), Replay::Unsafe));
    assert_eq!(ToolDeclaration::output(&declaration).max_bytes, Some(13));
    assert!(declaration.parameters.get("required").is_some());
}
