use maho_codemode::kernels::{
    js::{
        process_worker::crash_cause_from,
        run_queue::JavaScriptRunQueue,
        worker_host::{bridge_error, error_from_bridge},
    },
    shared::{subprocess_contract::KernelRunInput, subprocess_process::SubprocessProcess},
};
use serde_json::json;

#[test]
fn crash_preserves_error_and_elapsed_time_without_settling_waiting_cell() {
    // Given an active cell and a queued successor.
    let mut queue = JavaScriptRunQueue::default();
    let active = queue.enqueue(
        KernelRunInput {
            cell_id: "active".into(),
            code: String::new(),
            timeout_ms: None,
        },
        None,
        None,
    );
    let waiting = queue.enqueue(
        KernelRunInput {
            cell_id: "waiting".into(),
            code: String::new(),
            timeout_ms: None,
        },
        None,
        None,
    );
    queue.start_next(100.0);
    let cause = json!({"message":"worker failed","name":"TypeError","stack":"TypeError: worker failed\n at cell:1"});
    let error = error_from_bridge(&cause);

    // When the worker crashes after 42.6 ms of execution.
    let mut run = queue.release_active().expect("active run");
    JavaScriptRunQueue::settle_crashed(&mut run, &error, 142.6);

    // Then its structured error and rounded duration survive, and the successor waits.
    let result = active.borrow();
    let result = result.as_ref().expect("settled").as_ref().expect("result");
    assert_eq!(result["error"], cause);
    assert_eq!(result["durationMs"], 43.0);
    assert_eq!(result["ok"], false);
    assert!(result.get("kernelState").is_none());
    assert!(waiting.borrow().is_none());
    assert_eq!(queue.snapshot(), (None, vec!["waiting".into()]));
}

#[test]
fn process_crash_cause_requires_matching_token() {
    // Given stderr with a process-owned crash report.
    let stderr =
        "output\nsenpi-kernel-crash secret {\"message\":\"boom\",\"name\":\"RangeError\"}\n";
    // When the host reads that report with its own token.
    let cause = crash_cause_from(stderr, "secret").expect("crash cause");
    // Then the error survives and reports for another process are ignored.
    assert_eq!(bridge_error(&error_from_bridge(&cause)), cause);
    assert!(crash_cause_from(stderr, "other").is_none());
}

#[tokio::test]
async fn subprocess_exit_keeps_exit_code() {
    // Given a real Bun subprocess that exits before producing a result.
    let cwd = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let env = maho_ext_api::process_env::vars().into_iter().collect();
    let mut process =
        SubprocessProcess::spawn("bun", &["-e".into(), "process.exit(23)".into()], cwd, &env)
            .expect("Bun subprocess");
    // When the channel closes, await the exact closure with a bounded deadline.
    let error = tokio::time::timeout(std::time::Duration::from_secs(5), process.next_message())
        .await
        .expect("bounded exit")
        .expect_err("process exit");
    // Then the crash exposes the upstream exit identity.
    let error = bridge_error(&error);
    assert_eq!(error["name"], "JavaScriptWorkerExitedError");
    assert_eq!(error["message"], "JavaScript worker exited with code 23");
}

#[tokio::test]
async fn worker_error_reaches_host_with_name_and_stack() {
    let cwd = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut worker = maho_codemode::kernels::js::worker_host::WorkerHost::spawn(
        &cwd.join("tests/fixtures/crashing-worker.js"),
        cwd,
        1,
        maho_codemode::kernels::js::worker_host::JavaScriptKernelMode::Worker,
    )
    .expect("worker");
    let error = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let ready = worker.next_message().await.expect("ready");
        assert_eq!(ready["type"], "ready");
        worker
            .post_message(&json!({"type":"run"}))
            .await
            .expect("trigger crash");
        worker.next_message().await.expect_err("worker crash")
    })
    .await
    .expect("bounded worker crash");
    worker.terminate().await.expect("retire worker");
    let error = bridge_error(&error);
    assert_eq!(error["name"], "TypeError");
    assert_eq!(error["message"], "worker crash fixture");
    assert!(
        error["stack"]
            .as_str()
            .is_some_and(|stack| stack.contains("crashing-worker.js"))
    );
}

#[cfg(unix)]
#[test]
fn process_signal_keeps_signal_identity() {
    use maho_codemode::kernels::shared::subprocess_process::ProcessError;
    use std::os::unix::process::ExitStatusExt;
    // Given a child killed by SIGKILL.
    let error = ProcessError::ExitedWithStatus(std::process::ExitStatus::from_raw(9));
    // When the exit is converted to the cell's bridge error.
    let error = bridge_error(&error);
    // Then the signal is retained instead of becoming a generic channel closure.
    assert_eq!(error["name"], "JavaScriptWorkerExitedError");
    assert_eq!(
        error["message"],
        "JavaScript worker exited with signal 9 (code -1)"
    );
}
