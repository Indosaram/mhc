#[tokio::test]
async fn sdk_executor_forwards_partial_updates_to_agent_callback() {
    use std::sync::{Arc, Mutex};
    use maho_core::{sdk::{create_agent_session, CreateAgentSessionOptions}, session_manager::SessionManager};
    let dir = tempfile::tempdir().expect("isolated session");
    let cwd = dir.path().to_string_lossy().into_owned();
    let model = serde_json::from_value(serde_json::json!({
        "id":"fixture", "name":"fixture", "api":"faux", "provider":"faux",
        "baseUrl":"", "reasoning":false, "input":[], "contextWindow":128000, "maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
    })).expect("model");
    let custom = maho_tools::definition::ToolDefinition::new("updates", "fixture", serde_json::json!({"type":"object"}),
        Arc::new(|call| Box::pin(async move {
            if let Some(update) = call.on_update { update(maho_tools::definition::ToolResult::text("PARTIAL_FIXTURE"))?; }
            Ok(maho_tools::definition::ToolResult::text("FINAL_FIXTURE"))
        })));
    let session = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        model: Some(model), session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
        tools: Some(vec!["updates".to_owned()]), custom_tools: vec![custom], ..Default::default()
    }).await.expect("session").session;
    let tool = session.agent().state().tools().iter().find(|tool| tool.name() == "updates").expect("registered tool").clone();
    let updates = Arc::new(Mutex::new(Vec::new()));
    let observed = updates.clone();
    let result = (tool.execute)("call".into(), serde_json::json!({}), None, Some(Arc::new(move |result| {
        observed.lock().expect("updates").push(maho_ai::utils::text::content_text(&result.content, ""));
    }))).await;
    session.dispose().await;
    assert_eq!(maho_ai::utils::text::content_text(&result.content, ""), "FINAL_FIXTURE");
    assert_eq!(*updates.lock().expect("updates"), ["PARTIAL_FIXTURE"]);
}

#[tokio::test]
async fn sdk_executor_preserves_cancellation_through_shared_wrapper() {
    use std::sync::{Arc, Mutex};
    use maho_core::{sdk::{create_agent_session, CreateAgentSessionOptions}, session_manager::SessionManager};
    let dir = tempfile::tempdir().expect("isolated session");
    let cwd = dir.path().to_string_lossy().into_owned();
    let model = serde_json::from_value(serde_json::json!({
        "id":"fixture", "name":"fixture", "api":"faux", "provider":"faux",
        "baseUrl":"", "reasoning":false, "input":[], "contextWindow":128000, "maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
    })).expect("model");
    let (entered, observed) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered)));
    let custom = maho_tools::definition::ToolDefinition::new("cancel", "fixture", serde_json::json!({"type":"object"}),
        Arc::new(move |call| {
            let entered = entered.clone();
            Box::pin(async move {
                entered.lock().expect("entry signal").take().expect("single call").send(()).expect("observer");
                call.signal.cancelled().await;
                call.signal.check()?;
                Ok(maho_tools::definition::ToolResult::text("UNREACHABLE"))
            })
        }));
    let session = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        model: Some(model), session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
        tools: Some(vec!["cancel".to_owned()]), custom_tools: vec![custom], ..Default::default()
    }).await.expect("session").session;
    let tool = session.agent().state().tools().iter().find(|tool| tool.name() == "cancel").expect("registered tool").clone();
    let controller = maho_ai::utils::abort::AbortController::new();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let execute = (tool.execute)("call".into(), serde_json::json!({}), Some(controller.signal()), None);
        let abort = async { observed.await.expect("executor entered"); controller.abort(None); };
        let (result, ()) = tokio::join!(execute, abort);
        result
    }).await;
    session.dispose().await;
    assert_eq!(outcome.expect("bounded cancellation").is_error, Some(true));
}
