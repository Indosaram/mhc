//! Hosted here, not in `maho-core`, because four of the nine tools live in extension crates that
//! depend on `maho-core`; only this crate links all nine into one registry.

use std::sync::Arc;

use maho_core::sdk::{CreateAgentSessionOptions, create_agent_session};
use maho_core::session_manager::SessionManager;
use maho_core::session_work_barrier::ToolWorkspaceEffect;

const READ_ONLY: [&str; 9] = [
    "read",
    "grep",
    "find",
    "glob",
    "ls",
    "todo",
    "web_search",
    "webfetch",
    "tool_search",
];

#[tokio::test]
async fn every_documented_read_tool_declares_read() {
    let dir = tempfile::tempdir().expect("temporary workspace");
    let cwd = dir.path().to_string_lossy().into_owned();
    let agent_dir = dir.path().join("agent");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let factories =
        maho_cli::cli::default_extensions::factories(sender, Arc::new(std::sync::OnceLock::new()));
    let session = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()),
        agent_dir: Some(agent_dir.to_string_lossy().into_owned()),
        session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
        extension_factories: factories,
        auto_title_sessions: Some(false),
        ..Default::default()
    })
    .await
    .expect("session with the default builtin factories")
    .session;

    for name in READ_ONLY {
        assert_eq!(
            session.get_tool_workspace_effect(name),
            ToolWorkspaceEffect::Read,
            "{name} must resolve read-only from its definition (an unresolved name would fall back to Write)"
        );
    }
    for name in ["bash", "edit", "write"] {
        assert_eq!(
            session.get_tool_workspace_effect(name),
            ToolWorkspaceEffect::Write,
            "{name} must keep the conservative write default"
        );
    }
    session.dispose().await;
}
