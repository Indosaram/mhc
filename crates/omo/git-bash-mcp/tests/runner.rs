#![cfg(unix)]

//! Port of the pinned `runner.test.ts`, plus the timeout-kill and per-call cleanup boundaries the
//! SC-P1 contract requires (`mkdtempSync` output files removed after read, `setTimeout` kill).
//!
//! Unix-only: the pinned test branches to a `bash.cmd` fixture on Windows, and the native Windows
//! runtime is unavailable on this host (recorded as blocked, not mocked-equivalent).

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use git_bash_mcp::GitBashRunInput;
use git_bash_mcp::GitBashRunResult;
use git_bash_mcp::run_git_bash_command;
use git_bash_mcp::run_git_bash_command_in_dir;
use pretty_assertions::assert_eq;

fn write_fake_bash(directory: &Path, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;

    let path = directory.join("bash");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write fake bash");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path.to_string_lossy().into_owned()
}

fn inherited_env(extra: &[(&str, &str)]) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    for (key, value) in extra {
        env.insert((*key).to_owned(), (*value).to_owned());
    }
    env
}

#[test]
fn run_invokes_bash_with_lc_and_captures_output() {
    let directory = tempfile::tempdir().expect("temp dir");
    let argv_path = directory.path().join("argv.txt");
    let argv_text = argv_path.to_string_lossy().into_owned();
    let bash_path = write_fake_bash(
        directory.path(),
        concat!(
            "printf '%s\\n' \"$@\" > \"$FAKE_BASH_ARGV_PATH\"\n",
            "printf 'fake stdout\\n'\n",
            "printf 'fake stderr\\n' >&2\n",
            "exit 7",
        ),
    );
    let input = GitBashRunInput {
        bash_path,
        command: "printf ok".to_owned(),
        cwd: Some(directory.path().to_string_lossy().into_owned()),
        timeout_ms: 5_000,
        env: Some(inherited_env(&[("FAKE_BASH_ARGV_PATH", argv_text.as_str())])),
    };

    let result = run_git_bash_command(&input).expect("run");

    assert_eq!(
        std::fs::read_to_string(&argv_path)
            .expect("argv")
            .replace("\r\n", "\n"),
        "-lc\nprintf ok\n"
    );
    assert_eq!(
        result,
        GitBashRunResult {
            exit_code: Some(7),
            stdout: "fake stdout\n".to_owned(),
            stderr: "fake stderr\n".to_owned(),
            timed_out: false,
        }
    );
}

#[test]
fn run_removes_the_per_call_output_directory() {
    let base = tempfile::tempdir().expect("base temp dir");
    let directory = tempfile::tempdir().expect("script temp dir");
    let bash_path = write_fake_bash(directory.path(), "printf 'ok\\n'");
    let input = GitBashRunInput {
        bash_path,
        command: "printf ok".to_owned(),
        cwd: None,
        timeout_ms: 5_000,
        env: None,
    };

    let result = run_git_bash_command_in_dir(&input, base.path()).expect("run");

    assert_eq!(result.stdout, "ok\n");
    assert_eq!(result.exit_code, Some(0));
    assert!(
        std::fs::read_dir(base.path())
            .expect("read base")
            .next()
            .is_none(),
        "the per-call output directory must be removed after read"
    );
}

#[test]
fn run_kills_the_child_when_the_timeout_elapses() {
    let directory = tempfile::tempdir().expect("temp dir");
    let bash_path = write_fake_bash(
        directory.path(),
        "printf 'partial\\n'\nexec sleep 30\nprintf 'never\\n'",
    );
    let input = GitBashRunInput {
        bash_path,
        command: "sleep".to_owned(),
        cwd: None,
        timeout_ms: 500,
        env: None,
    };

    let started = Instant::now();
    let result = run_git_bash_command(&input).expect("run");
    let elapsed = started.elapsed();

    assert!(result.timed_out, "the timeout must report timedOut");
    assert!(
        elapsed < Duration::from_secs(10),
        "the timeout must kill the child promptly; took {elapsed:?}"
    );
    assert_eq!(result.stdout, "partial\n");
    assert!(!result.stdout.contains("never"));
}

#[test]
fn run_removes_the_output_directory_after_a_timeout_kill() {
    let base = tempfile::tempdir().expect("base temp dir");
    let directory = tempfile::tempdir().expect("script temp dir");
    let bash_path = write_fake_bash(directory.path(), "exec sleep 30");
    let input = GitBashRunInput {
        bash_path,
        command: "sleep".to_owned(),
        cwd: None,
        timeout_ms: 500,
        env: None,
    };

    let result = run_git_bash_command_in_dir(&input, base.path()).expect("run");

    assert!(result.timed_out);
    assert!(
        std::fs::read_dir(base.path())
            .expect("read base")
            .next()
            .is_none(),
        "a timed-out call must still remove its output directory"
    );
}

#[test]
fn run_reports_a_spawn_failure() {
    let base = tempfile::tempdir().expect("base temp dir");
    let input = GitBashRunInput {
        bash_path: "/nonexistent/omo-git-bash-fixture".to_owned(),
        command: "printf ok".to_owned(),
        cwd: None,
        timeout_ms: 5_000,
        env: None,
    };

    let error = run_git_bash_command_in_dir(&input, base.path()).expect_err("spawn must fail");

    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(
        std::fs::read_dir(base.path())
            .expect("read base")
            .next()
            .is_none(),
        "a failed spawn must not leave its output directory behind"
    );
}
