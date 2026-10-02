#![cfg(unix)]
use maho_ext_api::{AbortSignal, ExecOptions};
use maho_ext_host::exec::exec_command;
use std::{path::Path, time::Duration};

#[tokio::test]
async fn captures_output_and_nonzero_exit() {
    let result = exec_command("sh", &["-c".into(), "printf out; printf err >&2; exit 3".into()], Path::new("/tmp"), Default::default()).await;
    assert_eq!((result.stdout.as_str(), result.stderr.as_str(), result.code, result.killed), ("out", "err", 3, false));
}

#[tokio::test]
async fn pre_aborted_exec_reports_termination() {
    let signal = AbortSignal::default();
    signal.abort();
    let result = exec_command("sh", &["-c".into(), "while :; do :; done".into()], Path::new("/tmp"), ExecOptions { signal: Some(signal), ..Default::default() }).await;
    assert!(result.killed);
}

#[tokio::test]
async fn quiet_inherited_pipe_releases_after_exit_idle_grace() {
    let result = tokio::time::timeout(Duration::from_secs(3), exec_command("sh", &["-c".into(), "sleep 60 & printf '%s' $!".into()], Path::new("/tmp"), Default::default())).await.expect("inherited pipe must not hold execution open");
    let pid = result.stdout.parse::<i32>().expect("background process id");
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL).expect("terminate inherited pipe owner");
    assert_eq!(result.code, 0);
    assert!(!result.killed);
}

#[tokio::test]
async fn timeout_terminates_running_process() {
    let result = tokio::time::timeout(Duration::from_secs(3), exec_command("sh", &["-c".into(), "while :; do :; done".into()], Path::new("/tmp"), ExecOptions { timeout_ms: Some(50), ..Default::default() })).await.expect("timeout must terminate the process");
    assert!(result.killed);
}
