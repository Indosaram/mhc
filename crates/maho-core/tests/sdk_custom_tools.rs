#[tokio::test]
async fn sdk_custom_tools_obey_default_suppression_and_explicit_allowlist() {
    use maho_core::{sdk::{create_agent_session, CreateAgentSessionOptions, NoToolsMode}, session_manager::SessionManager};
    let mut mismatches = Vec::new();
    for (mode, explicit, exposure, excluded, enabled) in [
        (None, None, None, false, true),
        (Some(NoToolsMode::Builtin), None, None, false, true),
        (Some(NoToolsMode::All), None, None, false, false),
        (None, Some(vec!["read".to_owned()]), None, false, false),
        (Some(NoToolsMode::All), Some(vec!["custom_fixture".to_owned()]), None, false, true),
        (None, None, Some(maho_ext_api::ToolExposure::Search), false, false),
        (None, None, Some(maho_ext_api::ToolExposure::Eval), false, true),
        (None, None, None, true, false),
    ] {
        let dir = tempfile::tempdir().expect("isolated session");
        let cwd = dir.path().to_string_lossy().into_owned();
        let model = serde_json::from_value(serde_json::json!({
            "id":"fixture", "name":"fixture", "api":"faux", "provider":"faux",
            "baseUrl":"", "reasoning":false, "input":[], "contextWindow":128000, "maxTokens":4096,
            "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
        })).expect("model");
        let mut custom = maho_tools::index::create_all_tool_definitions(dir.path(), Default::default())
            .remove("read").expect("read definition");
        custom.name = "custom_fixture".to_owned();
        custom.exposure = exposure;
        let session = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            model: Some(model), session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
            tools: explicit, no_tools: mode, custom_tools: vec![custom],
            exclude_tools: excluded.then(|| vec!["custom_fixture".to_owned()]), ..Default::default()
        }).await.expect("session").session;
        let active = session.get_active_tool_names().iter().any(|name| name == "custom_fixture");
        if active != enabled { mismatches.push((mode, active, enabled)); }
        if active {
            std::fs::write(dir.path().join("input.txt"), "CUSTOM_TOOL_EXECUTED").expect("input");
            let result = session.execute_tool("custom_fixture", serde_json::json!({"path":"input.txt"}), Default::default())
                .await.expect("custom executor");
            assert!(maho_ai::utils::text::content_text(&result.content, "").contains("CUSTOM_TOOL_EXECUTED"));
        }
        session.dispose().await;
    }
    assert!(mismatches.is_empty(), "custom activation mismatches: {mismatches:?}");
}
