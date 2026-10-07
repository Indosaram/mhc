//! senpi host-watchdog.ts / multi-session-host.ts lifetime bindings, ported at the behaviour level.
use std::collections::HashMap;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use maho_core::agent_session_runtime::{AgentSessionLaunchProfile, AgentSessionRuntime};
use maho_rpc::host_watchdog::{
    arm_host_watchdog, read_host_watchdog_config_from_brand_env, HostWatchdogConfig, HOST_WATCH_FD_ENV,
    HOST_WATCH_PPID_ENV,
};
use maho_rpc::multi_session_host::{
    run_multi_session_host_with_signals, HostCore, HostCoreOptions, HostRuntimeFactory, HostShutdownSignals,
};
use maho_rpc::session_event_writer::SessionWriterActor;
use maho_rpc::session_registry::RpcSessionLaunchProfile;

/// A core that never opens a session: this file exercises the host's own lifetime, not a runtime.
fn idle_host_core(agent_dir: &std::path::Path) -> Arc<HostCore> {
    let create_runtime: HostRuntimeFactory = Arc::new(
        |_profile: RpcSessionLaunchProfile| -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<AgentSessionRuntime, String>> + Send>,
        > { Box::pin(async { Err("this host opens no session in this test".to_owned()) }) },
    );
    let writer = Arc::new(SessionWriterActor::new(tokio::io::sink()));
    Arc::new(HostCore::new(
        HostCoreOptions {
            agent_dir: agent_dir.to_path_buf(),
            cwd: agent_dir.to_string_lossy().into_owned(),
            create_runtime,
            capabilities: Vec::new(),
            close_grace_ms: 10_000,
        },
        writer,
    ))
}

#[tokio::test]
async fn inherited_pipe_eof_fires_before_the_private_directory_is_removed() {
    let temp = tempfile::tempdir().unwrap();
    let scratch = temp.path().join("scratch");
    std::fs::create_dir(&scratch).unwrap();
    // The supervisor's half of the lifetime pipe: the read end is what the child inherits.
    let (read_end, write_end): (OwnedFd, OwnedFd) = rustix::pipe::pipe().unwrap();
    let descriptor = u64::try_from(read_end.as_raw_fd()).unwrap();
    let config = HostWatchdogConfig {
        fd: Some(descriptor),
        ppid: None,
        scratch_dir: Some(scratch.clone()),
        cleanup_paths: None,
        public_socket: None,
    };
    let capture_saw_directory = Arc::new(AtomicBool::new(false));
    let capture_flag = capture_saw_directory.clone();
    let capture_scratch = scratch.clone();
    let capture = async move {
        capture_flag.store(capture_scratch.exists(), Ordering::SeqCst);
        Ok(())
    };
    let (reason_tx, mut reason_rx) = tokio::sync::mpsc::unbounded_channel();
    let armed = tokio::spawn(arm_host_watchdog(Some(config), capture, move |reason| {
        let _ = reason_tx.send(reason);
    }));
    // The supervisor died: only its write end closes, never a written byte.
    drop(write_end);
    let reason = tokio::time::timeout(Duration::from_secs(5), reason_rx.recv())
        .await
        .expect("the watchdog fired")
        .expect("a reason was delivered");
    armed.await.unwrap();
    assert!(reason.contains("closed"), "unexpected reason {reason}");
    assert!(capture_saw_directory.load(Ordering::SeqCst), "the capture must run before cleanup");
    assert!(!scratch.exists(), "the private directory is removed");
}

#[tokio::test(start_paused = true)]
async fn a_gone_supervisor_pid_fires_the_watchdog_on_the_next_tick() {
    let temp = tempfile::tempdir().unwrap();
    let scratch = temp.path().join("scratch");
    std::fs::create_dir(&scratch).unwrap();
    // u32::MAX is never a live pid, so this is the reparenting path with no timing luck.
    let config = HostWatchdogConfig {
        fd: None,
        ppid: Some(u32::MAX),
        scratch_dir: Some(scratch.clone()),
        cleanup_paths: None,
        public_socket: None,
    };
    let (reason_tx, mut reason_rx) = tokio::sync::mpsc::unbounded_channel();
    let armed = tokio::spawn(arm_host_watchdog(Some(config), async { Ok(()) }, move |reason| {
        let _ = reason_tx.send(reason);
    }));
    let reason = tokio::time::timeout(Duration::from_secs(60), reason_rx.recv())
        .await
        .expect("the watchdog fired")
        .expect("a reason was delivered");
    armed.await.unwrap();
    assert!(reason.contains("is gone"), "unexpected reason {reason}");
    assert!(!scratch.exists());
}

#[test]
fn the_watchdog_binding_is_read_from_the_supervisor_handoff_names() {
    assert!(read_host_watchdog_config_from_brand_env(&HashMap::new()).is_none());
    let env = HashMap::from([
        (HOST_WATCH_FD_ENV.to_owned(), "7".to_owned()),
        (HOST_WATCH_PPID_ENV.to_owned(), "42".to_owned()),
    ]);
    let config = read_host_watchdog_config_from_brand_env(&env).expect("the binding is present");
    assert_eq!(config.fd, Some(7));
    assert_eq!(config.ppid, Some(42));
}

#[tokio::test]
async fn a_shared_host_answers_the_protocol_then_exits_on_its_empty_window() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("host.sock");
    let socket_text = socket.to_string_lossy().into_owned();
    let core = idle_host_core(temp.path());
    let signals = HostShutdownSignals { watchdog: None, empty_exit_ms: Some(2_000.0) };
    let served = tokio::spawn(run_multi_session_host_with_signals(core, Some(socket_text.clone()), signals));
    // Subscribe to the host's answer, then to its exit.
    let protocol = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(protocol) = maho_rpc::host_probe::probe_protocol_info(&socket_text, 2_000).await {
                return protocol;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the shared host answered the protocol probe");
    assert_eq!(protocol.protocol_version, 1.0);
    assert!(!protocol.server_version.is_empty());
    let result = tokio::time::timeout(Duration::from_secs(20), served).await.expect("the host exited").unwrap();
    assert!(result.is_ok());
    assert!(!socket.exists(), "the host removes the endpoint it bound");
}

#[test]
fn a_launch_profile_carries_the_session_cwd() {
    let profile = AgentSessionLaunchProfile {
        cwd: "/tmp/host-session".to_owned(),
        permission_preset: None,
        creation_model: None,
        initial_thinking_level: None,
        auto_title: None,
    };
    assert_eq!(profile.cwd, "/tmp/host-session");
}
