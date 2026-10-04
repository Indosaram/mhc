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
