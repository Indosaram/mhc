#[tokio::test]
async fn explicit_tool_selection_takes_precedence_over_default_suppression() {
    use maho_core::{sdk::{create_agent_session, CreateAgentSessionOptions, NoToolsMode}, session_manager::SessionManager};
    let dir = tempfile::tempdir().expect("isolated session");
    let cwd = dir.path().to_string_lossy().into_owned();
    let model = serde_json::from_value(serde_json::json!({
        "id":"fixture", "name":"fixture", "api":"faux", "provider":"faux",
        "baseUrl":"", "reasoning":false, "input":[], "contextWindow":128000, "maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
    })).expect("model");
    let session = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        model: Some(model), session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
        tools: Some(vec!["read".to_owned()]), no_tools: Some(NoToolsMode::All), ..Default::default()
    }).await.expect("session").session;
    let active = session.get_active_tool_names();
    session.dispose().await;
    assert_eq!(active, ["read"], "explicit allowlist precedes default suppression");
}

#[tokio::test]
async fn initial_tools_follow_settings_explicit_selection_and_exclusions() {
    use maho_core::{sdk::{create_agent_session, CreateAgentSessionOptions}, session_manager::SessionManager,
        settings_manager::{SettingsManager, InMemorySettingsStorage}};
    let mut mismatches = Vec::new();
    for (configured, explicit, excluded, expected) in [
        (None, None, None, vec!["bash", "edit", "grep", "read", "write"]),
        (Some(vec!["grep"]), None, None, vec!["grep"]),
        (Some(vec!["grep"]), Some(vec!["read"]), None, vec!["read"]),
        (None, Some(vec!["read", "grep"]), Some(vec!["read"]), vec!["grep"]),
    ] {
        let dir = tempfile::tempdir().expect("isolated session");
        let cwd = dir.path().to_string_lossy().into_owned();
        let model = serde_json::from_value(serde_json::json!({
            "id":"fixture", "name":"fixture", "api":"faux", "provider":"faux",
            "baseUrl":"", "reasoning":false, "input":[], "contextWindow":128000, "maxTokens":4096,
            "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
        })).expect("model");
        let mut settings = SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), false);
        if let Some(names) = configured {
            settings.apply_overrides(&serde_json::Map::from_iter([("defaultTools".to_owned(), serde_json::json!(names))]));
        }
        let owned = |names: Vec<&str>| names.into_iter().map(str::to_owned).collect();
        let session = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            model: Some(model), session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
            settings_manager: Some(settings), tools: explicit.map(owned), exclude_tools: excluded.map(owned),
            ..Default::default()
        }).await.expect("session").session;
        let mut active = session.get_active_tool_names();
        session.dispose().await;
        active.sort();
        if active != expected { mismatches.push((active, expected)); }
    }
    assert!(mismatches.is_empty(), "selection mismatches: {mismatches:?}");
}
