use std::sync::Arc;

#[tokio::test]
async fn native_renderer_inventory_includes_inactive_registration_and_replacement() {
    let dir = tempfile::tempdir().expect("isolated SDK");
    let cwd = dir.path().to_string_lossy().into_owned();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "native-renderer-fixture".into(), source_info: Default::default(),
        factory: Arc::new(|api| {
            let registration = api.register_tool_with_renderers(maho_ext_api::ToolDefinition::new("card_fixture", "native card",
                serde_json::json!({"type":"object","properties":{}}), Arc::new(|_| Box::pin(async { Ok(maho_ext_api::ToolResult::text("card")) }))),
                maho_ext_api::ToolRenderers::<(), serde_json::Value> { render_call: None, render_result: None });
            Box::pin(async move { registration })
        }),
    };
    let model = serde_json::from_value(serde_json::json!({"id":"fixture","name":"fixture","api":"faux","provider":"faux",
        "baseUrl":"","reasoning":false,"input":[],"contextWindow":128000,"maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}})).expect("model");
    let session = maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()), model: Some(model),
        tools: Some(Vec::new()), extension_factories: vec![factory],
        session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)), ..Default::default()
    }).await.expect("native SDK").session;
    session.set_active_tools_by_name(Vec::new());
    let first = session.native_tool_renderers_snapshot::<(), serde_json::Value>().await;
    let replacement = tokio::time::timeout(std::time::Duration::from_secs(5), session.new_session(None)).await;
    let next = session.native_tool_renderers_snapshot::<(), serde_json::Value>().await;
    session.dispose().await;
    replacement.expect("bounded replacement").expect("replacement");
    assert!(first.contains_key("card_fixture"));
    assert!(next.contains_key("card_fixture"));
    assert!(!Arc::ptr_eq(&first["card_fixture"], &next["card_fixture"]));
}
