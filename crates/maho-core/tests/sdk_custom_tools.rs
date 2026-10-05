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

#[tokio::test]
async fn native_factory_tools_cannot_escape_sdk_allowlist_or_exclusions() {
    use std::sync::Arc;
    for (all, explicit, excluded, expected) in [
        (false, false, false, true), (true, false, false, false),
        (false, true, false, false), (false, false, true, false),
    ] {
        let dir = tempfile::tempdir().expect("isolated native tool selection");
        let cwd = dir.path().to_string_lossy().into_owned();
        let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
            path: "selection-fixture".into(), source_info: Default::default(),
            factory: Arc::new(|api| {
                api.register_tool(maho_ext_api::ToolDefinition::new("native_fixture", "fixture", serde_json::json!({"type":"object"}),
                    Arc::new(|_| Box::pin(async { Ok(maho_ext_api::ToolResult::text("native execution")) }))));
                Box::pin(async { Ok(()) })
            }),
        };
        let model = serde_json::from_value(serde_json::json!({"id":"fixture","name":"fixture","api":"faux","provider":"faux",
            "baseUrl":"","reasoning":false,"input":[],"contextWindow":128000,"maxTokens":4096,
            "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}})).expect("model");
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), maho_core::sdk::create_agent_session(
            maho_core::sdk::CreateAgentSessionOptions {
                cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()), model: Some(model),
                session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
                no_tools: all.then_some(maho_core::sdk::NoToolsMode::All), tools: explicit.then(|| vec!["read".into()]),
                exclude_tools: excluded.then(|| vec!["native_fixture".into()]), extension_factories: vec![factory], ..Default::default()
            })).await.expect("bounded native selection").expect("SDK");
        let active = result.session.get_active_tool_names().iter().any(|name| name == "native_fixture");
        let registered = result.session.get_tool_definition("native_fixture").is_some();
        let execution = tokio::time::timeout(std::time::Duration::from_secs(5),
            result.session.execute_tool("native_fixture", serde_json::json!({}), Default::default())).await;
        result.session.dispose().await;
        assert_eq!(active, expected, "all={all}, explicit={explicit}, excluded={excluded}");
        assert_eq!(registered, expected, "disallowed tools must not remain discoverable");
        let execution = execution.expect("bounded native execution admission");
        assert_eq!(execution.is_ok(), expected, "disallowed executors must not be callable");
        if let Ok(execution) = execution {
            assert_eq!(maho_ai::utils::text::content_text(&execution.content, ""), "native execution");
        }
    }
}
