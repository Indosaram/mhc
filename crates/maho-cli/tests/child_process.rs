#![cfg(unix)]
use maho_cli::utils::child_process::*;
#[cfg(unix)]
#[tokio::test]
async fn drains_output_and_observes_real_exit() {
    let mut child = tokio::process::Command::new("sh").args(["-c", "printf stdout; printf stderr >&2; exit 7"]).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    let mut out = Vec::new(); let mut err = Vec::new();
    let status = wait_for_child_process(&mut child, Default::default(), |stderr, bytes| { if stderr { err.extend_from_slice(bytes); } else { out.extend_from_slice(bytes); } }).await.unwrap();
    assert_eq!(status, Some(7));
    assert_eq!(out, b"stdout"); assert_eq!(err, b"stderr");
}
#[cfg(unix)]
#[tokio::test]
async fn preaborted_signal_releases_pipes_and_observes_exit() {
    let controller = maho_ai::utils::abort::AbortController::new(); controller.abort(None);
    let signal = controller.signal();
    let mut child = tokio::process::Command::new("sh").args(["-c", "exit 0"]).stdout(std::process::Stdio::piped()).spawn().unwrap();
    assert_eq!(wait_for_child_process(&mut child, WaitForChildProcessOptions { signal: Some(&signal), ..Default::default() }, |_, _| {}).await.unwrap(), Some(0));
}

#[cfg(unix)]
#[tokio::test]
async fn absent_pipes_wait_for_exit_without_reading() {
    let mut child = tokio::process::Command::new("sh").args(["-c", "exit 7"])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    let status = wait_for_child_process(&mut child, Default::default(), |_, _| panic!("null pipes have no output")).await.unwrap();
    assert_eq!(status, Some(7));
}

// senpi bash-abort-hang.test.ts, at the `waitForChildProcess` level.
#[cfg(unix)]
#[tokio::test]
async fn upstream_abort_exit_grace_releases_a_process_that_never_exits() {
    let mut child = tokio::process::Command::new("sh").args(["-c", "while :; do sleep 1; done"])
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().unwrap();
    let controller = maho_ai::utils::abort::AbortController::new();
    let signal = controller.signal();
    controller.abort(None);
    let wait = wait_for_child_process(&mut child, WaitForChildProcessOptions { signal: Some(&signal), abort_exit_grace_ms: 300 }, |_, _| {});
    let status = tokio::time::timeout(std::time::Duration::from_secs(8), wait).await.expect("abort exit grace releases the wait").unwrap();
    assert_eq!(status, None);
    assert!(child.try_wait().unwrap().is_none(), "the wait gave up on the process, not the other way round");
    child.kill().await.unwrap();
}
#[cfg(unix)]
#[tokio::test]
async fn upstream_abort_releases_the_wait_while_an_escaped_descendant_keeps_writing() {
    // The shell leaves a chattering grandchild behind on the inherited stdout pipe, then idles.
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("orphan.pid");
    let script = format!("(while :; do echo orphan-tick; sleep 0.04; done) & echo $! > '{}'; while :; do sleep 1; done", pid_file.display());
    let mut child = tokio::process::Command::new("sh").args(["-c", &script])
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().unwrap();
    let controller = maho_ai::utils::abort::AbortController::new();
    let signal = controller.signal();
    let pid = child.id().expect("pid");
    let wait = wait_for_child_process(&mut child, WaitForChildProcessOptions { signal: Some(&signal), abort_exit_grace_ms: 5000 }, |_, bytes| {
        if String::from_utf8_lossy(bytes).contains("orphan-tick") && !signal.aborted() {
            // The caller kills the direct child on abort (the escaped grandchild survives).
            let _ = std::process::Command::new("kill").args(["-9", &pid.to_string()]).status();
            controller.abort(None);
        }
    });
    let released = tokio::time::timeout(std::time::Duration::from_secs(8), wait).await;
    if let Ok(text) = std::fs::read_to_string(&pid_file) {
        let _ = std::process::Command::new("kill").args(["-9", text.trim()]).status();
    }
    assert!(released.expect("the wait released despite the chattering orphan").is_ok());
}
