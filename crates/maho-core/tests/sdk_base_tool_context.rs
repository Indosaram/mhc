use std::sync::{Arc, Mutex};

#[tokio::test]
async fn sdk_custom_tool_receives_live_context_after_native_replacement() {
    let directory = tempfile::tempdir().expect("isolated SDK");
    let cwd = directory.path().to_string_lossy().into_owned();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let captured = observed.clone();
    let definition = maho_ext_api::ToolDefinition::new("context_fixture", "Inspect SDK tool context",
        serde_json::json!({"type":"object","properties":{}}), Arc::new(move |call| {
            let context = call.context.expect("SDK base tool context");
            assert!(context.take_approved_monitor_parent(call.id, &call.params).is_err());
            captured.lock().expect("observations").push((context.cwd().to_path_buf(),
                context.session_manager().session_id().to_owned(), context.model().expect("live model").id.clone()));
            Box::pin(async { Ok(maho_ext_api::ToolResult::text("context observed")) })
        }));
    let model = serde_json::from_value(serde_json::json!({
        "id":"fixture","name":"fixture","api":"faux","provider":"faux","baseUrl":"",
        "reasoning":false,"input":[],"contextWindow":128000,"maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
    })).expect("model");
    let session = maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(directory.path().join("agent").to_string_lossy().into_owned()),
        model: Some(model), custom_tools: vec![definition],
        session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
        ..Default::default()
    }).await.expect("SDK session").session;
    let initial_id = session.session_id();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        session.execute_tool("context_fixture", serde_json::json!({}), Default::default()).await.map_err(|error| error.message)?;
        session.new_session(None).await?;
        session.execute_tool("context_fixture", serde_json::json!({}), Default::default()).await.map_err(|error| error.message)?;
        Ok::<_, String>(session.session_id())
    }).await;
    session.dispose().await;
    let replacement_id = result.expect("bounded execution and replacement").expect("live context");
    let observed = observed.lock().expect("observations");
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0], (directory.path().to_path_buf(), initial_id.clone(), "fixture".into()));
    assert_eq!(observed[1], (directory.path().to_path_buf(), replacement_id.clone(), "fixture".into()));
    assert_ne!(initial_id, replacement_id);
}
