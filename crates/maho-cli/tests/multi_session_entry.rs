//! The multi-session RPC entry (senpi `main.ts` `appMode === "rpc" && parsed.multiSession`): the
//! CLI configuration it is built from, the per-open profile mapping the host factory applies, and
//! the shared host the entry actually serves.
use std::time::Duration;

use maho_cli::cli::args::{Args, Mode};
use maho_cli::cli::host_runtime::{create_host_core, open_profile_session_manager, CliRuntimeConfiguration};
use maho_core::agent_session_runtime::AgentSessionLaunchProfile;
use maho_core::project_trust::AppMode;
use maho_rpc::multi_session_host::{run_multi_session_host_with_signals, HostShutdownSignals};
use maho_rpc::session_registry::RpcSessionLaunchProfile;

fn rpc_config(cwd: &str, agent_dir: &str) -> CliRuntimeConfiguration {
    CliRuntimeConfiguration { cwd: cwd.to_owned(), agent_dir: agent_dir.to_owned(), app_mode: AppMode::Rpc, ..Default::default() }
}

fn launch_profile(cwd: &str) -> AgentSessionLaunchProfile {
    AgentSessionLaunchProfile { cwd: cwd.to_owned(), permission_preset: None, creation_model: None, initial_thinking_level: None, auto_title: None }
}

#[test]
fn multi_session_rpc_arguments_build_an_rpc_configuration() {
    let parsed = Args { mode: Some(Mode::Rpc), multi_session: true, listen: Some("unix:///tmp/host.sock".to_owned()), ..Default::default() };
    let config = CliRuntimeConfiguration::from_parsed(&parsed, "/tmp/project", "/tmp/agent", AppMode::Rpc);
    assert_eq!(config.app_mode, AppMode::Rpc);
    assert_eq!(config.cwd, "/tmp/project");
    assert_eq!(config.agent_dir, "/tmp/agent");
    assert!(parsed.multi_session);
    assert_eq!(parsed.listen.as_deref(), Some("unix:///tmp/host.sock"));
}

#[test]
fn a_profile_session_path_and_durable_id_reach_the_session_manager() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().to_string_lossy().into_owned();
    let sessions = temp.path().join("sessions").to_string_lossy().into_owned();
    let mut config = rpc_config(&cwd, &cwd);
    config.session_dir = Some(sessions);
    let profile = RpcSessionLaunchProfile {
        runtime: launch_profile(&cwd),
        session_path: None,
        durable_session_id: Some("durable-session-1".to_owned()),
        session_kind: None,
        session_context: None,
    };
    let manager = open_profile_session_manager(&config, &profile);
    assert_eq!(manager.cwd(), cwd.as_str());
    assert_eq!(manager.session_id(), "durable-session-1");
}

#[test]
fn a_profile_session_path_opens_that_session_with_the_profile_cwd() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().to_string_lossy().into_owned();
    let sessions = temp.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let file = sessions.join("2026-01-01T00-00-00-000Z_path-session.jsonl");
    std::fs::write(
        &file,
        format!("{}\n", serde_json::json!({"type":"session","version":3,"id":"path-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":cwd})),
    )
    .unwrap();
    let mut config = rpc_config(&cwd, &cwd);
    config.session_dir = Some(sessions.to_string_lossy().into_owned());
    let profile = RpcSessionLaunchProfile {
        runtime: launch_profile(&cwd),
        session_path: Some(file.to_string_lossy().into_owned()),
        durable_session_id: None,
        session_kind: None,
        session_context: None,
    };
    let manager = open_profile_session_manager(&config, &profile);
    assert_eq!(manager.session_id(), "path-session");
    assert_eq!(manager.cwd(), cwd.as_str());
}

#[tokio::test]
async fn a_cli_built_host_answers_the_protocol_and_exits_on_its_empty_window() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().to_string_lossy().into_owned();
    let socket = temp.path().join("rpc.sock");
    let socket_text = socket.to_string_lossy().into_owned();
    let core = create_host_core(rpc_config(&cwd, &cwd), &cwd, Some(socket_text.as_str()));
    let signals = HostShutdownSignals { watchdog: None, empty_exit_ms: Some(2_000.0) };
    let served = tokio::spawn(run_multi_session_host_with_signals(core, Some(socket_text.clone()), signals));
    // Subscribe to the host's answer, then to its exit; the retry is bounded, never a fixed sleep.
    let protocol = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(protocol) = maho_rpc::host_probe::probe_protocol_info(&socket_text, 2_000).await {
                return protocol;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the CLI-built host answered the protocol probe");
    assert_eq!(protocol.protocol_version, 1.0);
    assert!(!protocol.server_version.is_empty());
    let result = tokio::time::timeout(Duration::from_secs(20), served).await.expect("the host exited").unwrap();
    assert!(result.is_ok());
    assert!(!socket.exists(), "the host removes the endpoint it bound");
}
