//! Port of src/runner.test.ts (unix only: the scripts drive /bin/sh).
#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::{env, strings};
use pretty_assertions::assert_eq;
use tmux_core::{RunTmuxOptions, TmuxCommandResult, run_tmux_command};

const COUNTING_SCRIPT_PREFIX: &str = r#"counter_file="$1"; count=0; if [ -f "$counter_file" ]; then count=$(cat "$counter_file"); fi; count=$((count + 1)); printf '%s' "$count" > "$counter_file"; "#;

fn plain_env() -> RunTmuxOptions {
    RunTmuxOptions {
        environment: Some(env(&[])),
        ..RunTmuxOptions::default()
    }
}

fn read_count(path: &Path) -> u32 {
    std::fs::read_to_string(path).unwrap().parse().unwrap()
}

#[test]
fn cmux_socket_with_real_tmux_session_uses_requested_executable() {
    let options = RunTmuxOptions {
        environment: Some(env(&[
            ("CMUX_SOCKET_PATH", "/tmp/cmux.sock"),
            ("TMUX", "/private/tmp/tmux-501/default,123,0"),
        ])),
        ..RunTmuxOptions::default()
    };
    let result = run_tmux_command(
        "sh",
        &strings(&["-c", "printf '%s\\n' real-tmux"]),
        &options,
    );
    assert_eq!(result, TmuxCommandResult::new("real-tmux", "", 0));
}

#[test]
fn exit_zero_with_stdout_returns_trimmed_success() {
    let result = run_tmux_command(
        "sh",
        &strings(&["-c", "printf '%s\\n' '%42'"]),
        &plain_env(),
    );
    assert_eq!(
        result,
        TmuxCommandResult {
            success: true,
            output: "%42".into(),
            stdout: "%42".into(),
            stderr: String::new(),
            exit_code: 0,
        }
    );
}

#[test]
fn exit_one_with_stderr_populates_stderr() {
    let result = run_tmux_command(
        "sh",
        &strings(&["-c", "printf '%s\\n' 'some error' >&2; exit 1"]),
        &plain_env(),
    );
    assert!(!result.success);
    assert_eq!(result.stderr, "some error");
    assert_eq!(result.exit_code, 1);
}

#[test]
fn retry_two_with_nonzero_exit_runs_three_times() {
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("run.count");
    let script = format!("{COUNTING_SCRIPT_PREFIX}printf '%s\\n' 'temporary error' >&2; exit 1");
    let args = strings(&["-c", &script, "sh", counter.to_str().unwrap()]);
    let result = run_tmux_command(
        "sh",
        &args,
        &RunTmuxOptions {
            retry: 2,
            ..plain_env()
        },
    );
    assert!(!result.success);
    assert_eq!(result.stderr, "temporary error");
    assert_eq!(read_count(&counter), 3);
}

#[test]
fn retry_two_with_cant_find_pane_does_not_retry() {
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("run.count");
    let script =
        format!(r#"{COUNTING_SCRIPT_PREFIX}printf '%s\n' "can't find pane: %1" >&2; exit 1"#);
    let args = strings(&["-c", &script, "sh", counter.to_str().unwrap()]);
    let result = run_tmux_command(
        "sh",
        &args,
        &RunTmuxOptions {
            retry: 2,
            ..plain_env()
        },
    );
    assert!(!result.success);
    assert!(result.stderr.contains("can't find pane"));
    assert_eq!(read_count(&counter), 1);
}

#[test]
fn timeout_fifty_ms_with_sleeping_command_returns_timeout_failure() {
    let options = RunTmuxOptions {
        timeout_ms: Some(50),
        ..plain_env()
    };
    let result = run_tmux_command("sh", &strings(&["-c", "sleep 0.5"]), &options);
    assert!(!result.success);
    assert_eq!(result.exit_code, -1);
    assert!(result.stderr.contains("timeout"));
}

#[test]
fn trailing_newlines_are_trimmed_from_output() {
    let result = run_tmux_command(
        "sh",
        &strings(&["-c", "printf '%s\\n\\n' '%7'"]),
        &plain_env(),
    );
    assert_eq!(result.output, "%7");
    assert_eq!(result.stdout, "%7");
}

#[test]
fn backward_compat_success_and_output_fields_are_present() {
    let TmuxCommandResult {
        success, output, ..
    } = run_tmux_command("sh", &strings(&["-c", "printf '%s\\n' '%9'"]), &plain_env());
    assert!(success);
    assert_eq!(output, "%9");
}

#[test]
fn cmux_environment_delegates_through_tmux_compat_command() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("cmux.args");
    let cmux_path = dir.path().join("cmux");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\nprintf '%s\\n' '%42'",
        args_file.display()
    );
    std::fs::write(&cmux_path, script).unwrap();
    std::fs::set_permissions(&cmux_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let socket = dir.path().join("cmux.sock");
    let socket = socket.to_str().unwrap().to_owned();
    let environment: std::collections::HashMap<String, String> =
        [("CMUX_SOCKET_PATH".to_owned(), socket)].into();
    let options = RunTmuxOptions {
        environment: Some(std::sync::Arc::new(environment)),
        ..RunTmuxOptions::default()
    };

    // when
    let result = run_tmux_command(
        cmux_path.to_str().unwrap(),
        &strings(&["display-message", "-p", "#{pane_id}"]),
        &options,
    );

    // then
    assert_eq!(result, TmuxCommandResult::new("%42", "", 0));
    assert_eq!(
        std::fs::read_to_string(&args_file).unwrap(),
        "__tmux-compat\ndisplay-message\n-p\n#{pane_id}\n"
    );
}
