//! Real runner on unix hosts, using `/bin/sh` in place of `bash.exe` (the runner itself is
//! platform-neutral): output capture, timeout, workdir and temp-directory cleanup (spec §5.3).
#![cfg(unix)]

use maho_git_bash::RunRequest;
use maho_git_bash::runner::run_command;
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

fn request(command: &str, timeout_ms: u64) -> RunRequest {
    RunRequest {
        bash: "/bin/sh".into(),
        command: command.into(),
        workdir: None,
        timeout_ms,
    }
}

async fn run(request: &RunRequest, root: &tempfile::TempDir) -> maho_git_bash::RunOutput {
    let output = tokio::time::timeout(BOUND, run_command(request, Some(root.path())))
        .await
        .expect("finished within bound")
        .expect("ran");
    let left: Vec<_> = std::fs::read_dir(root.path()).expect("read root").collect();
    assert!(left.is_empty(), "temp directories left: {left:?}");
    output
}

#[tokio::test]
async fn runner_captures_streams_and_exit_code() {
    let root = tempfile::tempdir().expect("root");
    let output = run(&request("echo hi; echo err >&2; exit 3", 10_000), &root).await;
    assert_eq!(output.stdout, "hi\n");
    assert_eq!(output.stderr, "err\n");
    assert_eq!(output.exit_code, Some(3));
    assert!(!output.timed_out);
}

#[tokio::test]
async fn runner_kills_on_timeout_and_keeps_partial_output() {
    let root = tempfile::tempdir().expect("root");
    let output = run(&request("echo started; exec sleep 30", 200), &root).await;
    assert!(output.timed_out);
    assert_eq!(output.stdout, "started\n");
    assert_eq!(output.exit_code, None);
}

#[tokio::test]
async fn runner_honours_workdir_and_returns_large_output_whole() {
    let root = tempfile::tempdir().expect("root");
    let workdir = tempfile::tempdir().expect("workdir");
    let mut pwd = request("pwd", 10_000);
    pwd.workdir = Some(workdir.path().to_string_lossy().into_owned());
    let output = run(&pwd, &root).await;
    let expected = workdir.path().canonicalize().expect("canonical workdir");
    let reported = std::path::Path::new(output.stdout.trim_end())
        .canonicalize()
        .expect("canonical pwd");
    assert_eq!(reported, expected);

    let size = 2 * 1024 * 1024 + 7;
    let big = run(
        &request(&format!("head -c {size} /dev/zero | tr '\\000' 'a'"), 30_000),
        &root,
    )
    .await;
    assert_eq!(big.stdout.len(), size);
}

#[tokio::test]
async fn runner_decodes_invalid_utf8_lossily() {
    let root = tempfile::tempdir().expect("root");
    let output = run(&request(r"printf 'ok\377\n'", 10_000), &root).await;
    assert_eq!(output.stdout, "ok\u{fffd}\n");
}

#[tokio::test]
async fn runner_reports_spawn_failure_as_error_text() {
    let root = tempfile::tempdir().expect("root");
    let mut missing = request("true", 1000);
    missing.bash = "/nonexistent/bash.exe".into();
    let error = run_command(&missing, Some(root.path()))
        .await
        .expect_err("spawn fails");
    assert!(!error.is_empty());
}
