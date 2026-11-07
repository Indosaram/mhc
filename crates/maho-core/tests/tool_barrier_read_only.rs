use maho_core::session_manager::SessionManager;
use maho_core::session_work_barrier::{ToolBarrierSpec, ToolWorkspaceEffect};

async fn session_with_barrier() -> (tempfile::TempDir, maho_core::agent_session::AgentSession) {
    let dir = tempfile::tempdir().expect("temporary workspace");
    let cwd = dir.path().to_string_lossy().into_owned();
    let session = maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()),
        agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
        ..Default::default()
    })
    .await
    .expect("session")
    .session;
    session
        .arm_tool_barrier(ToolBarrierSpec {
            id: "barrier".into(),
            deadline_ms: 10_000,
        })
        .expect("arm");
    (dir, session)
}

// Only the tools this crate registers itself. `todo` and `web_search` declare the same effect
// in extension crates that depend on `maho-core`, so linking them here would be a cycle;
// `webfetch` and `tool_search` reach only `maho-tools` but are not registered by this crate's
// own session either, so they are out of scope for this test.
#[tokio::test]
async fn read_only_tools_barrier_writes() {
    let (_dir, session) = session_with_barrier().await;
    for name in ["read", "grep", "find", "glob", "ls"] {
        assert_eq!(
            session.get_tool_workspace_effect(name),
            ToolWorkspaceEffect::Read,
            "{name} must resolve read-only from its definition"
        );
    }
    session.dispose().await;
}

#[tokio::test]
async fn write_tools_do_not_barrier() {
    let (_dir, session) = session_with_barrier().await;
    for name in ["bash", "edit", "write"] {
        assert_eq!(
            session.get_tool_workspace_effect(name),
            ToolWorkspaceEffect::Write,
            "{name} must keep the conservative write default"
        );
    }
    session.dispose().await;
}
