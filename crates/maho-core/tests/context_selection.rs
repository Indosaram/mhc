#[tokio::test]
async fn context_selection_survives_rebuild_and_reaches_extension_options() {
    use maho_core::{sdk::{create_agent_session, CreateAgentSessionOptions}, session_manager::SessionManager};
    let dir = tempfile::tempdir().expect("isolated session");
    let cwd = dir.path().to_string_lossy().into_owned();
    let source = dir.path().join("AGENTS.md");
    std::fs::write(&source, "CONTEXT_SELECTION_FIXTURE").expect("context input");
    let model = serde_json::from_value(serde_json::json!({
        "id":"fixture", "name":"fixture", "api":"faux", "provider":"faux",
        "baseUrl":"", "reasoning":false, "input":[], "contextWindow":128000, "maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
    })).expect("model");
    let session = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        model: Some(model), session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
        tools: Some(Vec::new()), ..Default::default()
    }).await.expect("session").session;
    let actions = session.extension_context_actions();
    assert!(actions.get_system_prompt_options().context_files.iter().any(|file| file.path == source.to_string_lossy()));

    session.set_context_files_enabled(false);
    session.rebuild_system_prompt();
    assert!(actions.get_system_prompt_options().context_files.is_empty());
    assert!(!session.system_prompt().contains("CONTEXT_SELECTION_FIXTURE"));

    std::fs::write(&source, "CONTEXT_SELECTION_CHANGED").expect("changed context input");
    session.set_context_files_enabled(true);
    let options = actions.get_system_prompt_options();
    assert!(options.context_files.iter().any(|file| file.content == "CONTEXT_SELECTION_CHANGED"));
    assert!(session.system_prompt().contains("CONTEXT_SELECTION_CHANGED"));
    session.dispose().await;
}
