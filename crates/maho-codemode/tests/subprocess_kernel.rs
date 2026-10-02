use maho_codemode::bridge::protocol::BridgeConnectionConfig;
use maho_codemode::kernels::shared::subprocess_contract::*;
use maho_codemode::kernels::shared::subprocess_kernel::SubprocessKernel;

fn options() -> SubprocessKernelOptions {
    SubprocessKernelOptions {
        command: "python3".into(), args: vec!["-u".into(), concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/py/prelude.py").into()],
        cwd: env!("CARGO_MANIFEST_DIR").into(), env: None, session_env: None, on_message: None, session_id: "kernel-test".into(),
        connection: BridgeConnectionConfig { port: 1, token: "test".into(), local_roots: None, artifacts_dir: None, parallel_pool_width: None },
    }
}

#[tokio::test]
async fn default_callback_receives_ready_and_run_frames_but_explicit_callback_overrides() {
    use std::sync::{Arc,Mutex};
    let defaults=Arc::new(Mutex::new(Vec::new()));
    let observed=defaults.clone();
    let mut settings=options();
    settings.on_message=Some(Arc::new(move |message| observed.lock().unwrap().push(message.clone())));
    let kernel=SubprocessKernel::start(settings).await.unwrap();
    assert_eq!(defaults.lock().unwrap()[0]["type"],"ready");
    kernel.run_with_callbacks(KernelRunInput{cell_id:"fallback".into(),code:"1+1".into(),timeout_ms:Some(5000)},None,None).await.unwrap();
    assert!(defaults.lock().unwrap().iter().any(|frame|frame["type"]=="result"&&frame["cellId"]=="fallback"));
    let explicit=Arc::new(Mutex::new(Vec::new()));
    let observed=explicit.clone();
    kernel.run_with_callbacks(KernelRunInput{cell_id:"explicit".into(),code:"2+2".into(),timeout_ms:Some(5000)},Some(Arc::new(move |frame| observed.lock().unwrap().push(frame.clone()))),None).await.unwrap();
    kernel.close().await.unwrap();
    assert!(explicit.lock().unwrap().iter().any(|frame|frame["type"]=="result"&&frame["cellId"]=="explicit"));
    assert!(!defaults.lock().unwrap().iter().any(|frame|frame["cellId"]=="explicit"));
}

#[tokio::test]
async fn real_kernel_persistence_and_reset() {
    let kernel = SubprocessKernel::start(options()).await.unwrap();
    for (code, expected) in [("x = 7\nx", "7"), ("x + 1", "8")] {
        let result = kernel.run(KernelRunInput { cell_id: "c".into(), code: code.into(), timeout_ms: Some(5_000) }, |_| {}).await.unwrap();
        assert_eq!(result["valueRepr"], expected);
    }
    kernel.reset().await.unwrap();
    let result = kernel.run(KernelRunInput { cell_id: "reset".into(), code: "x".into(), timeout_ms: Some(5_000) }, |_| {}).await.unwrap();
    assert_eq!(result["ok"], false);
    kernel.close().await.unwrap();
    assert!(kernel.pid().is_none());
}

#[tokio::test]
async fn late_initialization_failure_settles_active_and_retires_process() {
    let mut settings=options();
    settings.args=vec!["-u".into(),"-c".into(),"import sys; sys.stdin.readline(); print('{\"type\":\"ready\"}',flush=True); sys.stdin.readline(); print('{\"type\":\"init-failed\",\"error\":{\"message\":\"late startup failure\"}}',flush=True); sys.stdin.readline()".into()];
    let kernel=SubprocessKernel::start(settings).await.unwrap();
    let pid=kernel.pid().unwrap();
    let result=tokio::time::timeout(std::time::Duration::from_secs(2),kernel.run_with_callbacks(KernelRunInput{cell_id:"failed".into(),code:"42".into(),timeout_ms:None},None,None)).await;
    kernel.close().await.unwrap();
    let result=result.expect("init-failed must settle the active run").unwrap();
    assert_eq!(result["ok"],false);
    assert!(result["error"]["message"].as_str().unwrap().contains("late startup failure"));
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}

#[tokio::test]
async fn startup_failure_cannot_be_revived_by_reset() {
    let mut settings=options();
    settings.args=vec!["-u".into(),"-c".into(),"import sys; sys.stdin.readline(); print('{\"type\":\"ready\"}',flush=True); sys.stdin.readline(); print('{\"type\":\"init-failed\",\"error\":{\"message\":\"permanent failure\"}}',flush=True); sys.stdin.readline()".into()];
    let kernel=SubprocessKernel::start(settings).await.unwrap();
    let result=kernel.run_with_callbacks(KernelRunInput{cell_id:"failed".into(),code:"42".into(),timeout_ms:None},None,None).await.unwrap();
    assert_eq!(result["ok"],false);
    let reset=kernel.reset().await;
    let run=kernel.run_with_callbacks(KernelRunInput{cell_id:"after-failure".into(),code:"42".into(),timeout_ms:None},None,None).await;
    kernel.close().await.unwrap();
    assert!(reset.is_err(),"source failClosed forbids reset from spawning a replacement");
    assert!(matches!(run,Err(maho_codemode::kernels::shared::subprocess_process::ProcessError::Closed)),"closed API must reject run admission rather than creating another failure result");
}

#[tokio::test]
async fn failed_run_frame_write_retires_owned_process_before_settlement() {
    let mut settings=options();
    settings.args=vec!["-u".into(),"-c".into(),"import os,sys,signal; sys.stdin.readline(); os.close(0); print('{\"type\":\"ready\"}',flush=True); signal.pause()".into()];
    let kernel=SubprocessKernel::start(settings).await.unwrap();
    let pid=kernel.pid().unwrap();
    let result=kernel.run_with_callbacks(KernelRunInput{cell_id:"failed-write".into(),code:"42".into(),timeout_ms:None},None,None).await.unwrap();
    let still_running=std::path::Path::new(&format!("/proc/{pid}")).exists();
    kernel.close().await.unwrap();
    assert_eq!(result["ok"],false);
    assert!(!still_running,"failed transport must retire its owned process before reporting settlement");
}

#[tokio::test]
async fn real_kernel_timeout_reaps_and_restarts() {
    let kernel = SubprocessKernel::start(options()).await.unwrap();
    let pid = kernel.pid().unwrap();
    let result = kernel.run(KernelRunInput { cell_id: "timeout".into(), code: "while True: pass".into(), timeout_ms: Some(20) }, |_| {}).await.unwrap();
    assert_eq!(result["ok"], false);
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    let result = kernel.run(KernelRunInput { cell_id: "after".into(), code: "1+1".into(), timeout_ms: Some(5_000) }, |_| {}).await.unwrap();
    assert_eq!(result["valueRepr"], "2");
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn queued_cancellation_preserves_active_and_interrupt_restarts_state() {
    use std::sync::Arc;
    let kernel=Arc::new(SubprocessKernel::start(options()).await.unwrap());
    let (started,mut starts)=tokio::sync::mpsc::unbounded_channel();
    let first_kernel=kernel.clone();
    let first=tokio::spawn(async move {first_kernel.run_with_callbacks(KernelRunInput {cell_id:"active".into(),code:"x=41\nwhile True: pass".into(),timeout_ms:None},None,Some(Arc::new(move || {started.send(()).expect("start receiver");}))).await});
    starts.recv().await.unwrap();
    let second=kernel.run_with_callbacks(KernelRunInput {cell_id:"queued".into(),code:"42".into(),timeout_ms:None},None,None);
    tokio::pin!(second);
    let cancel=async {kernel.cancel_queued("queued","test cancellation").await};
    let (result,cancelled)=tokio::join!(&mut second,cancel);
    assert!(cancelled);
    assert_eq!(result.unwrap()["durationMs"],0.0);
    assert_eq!(kernel.queue_snapshot().0.as_deref(),Some("active"));
    let old=kernel.pid().unwrap();
    assert!(!kernel.interrupt("test interruption",Some("active")).await.unwrap());
    assert_eq!(first.await.unwrap().unwrap()["ok"],false);
    assert!(!std::path::Path::new(&format!("/proc/{old}")).exists());
    let result=kernel.run(KernelRunInput {cell_id:"after".into(),code:"x".into(),timeout_ms:Some(5000)},|_|{}).await.unwrap();
    assert_eq!(result["ok"],false);
    kernel.close().await.unwrap();
}
