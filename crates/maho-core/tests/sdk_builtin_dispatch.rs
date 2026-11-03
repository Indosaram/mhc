//! senpi `wrapRegisteredTools(builtIns, ..., this._toolDispatcher)`: a built-in tool call runs through the session's
//! dispatcher, so its `tool_dispatched` observers see it.
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn built_in_tool_call_reaches_the_session_dispatchers_observers() {
    use maho_core::{
        sdk::{CreateAgentSessionOptions, create_agent_session},
        session_manager::SessionManager,
        tool_dispatch::tool_dispatcher::{ToolDispatchedInfo, ToolPlacement},
    };
    // Given: an SDK session with the built-in `ls` tool and an observer on its dispatcher.
    let dir = tempfile::tempdir().expect("isolated session");
    let cwd = dir.path().to_string_lossy().into_owned();
    let model = serde_json::from_value(serde_json::json!({
        "id":"fixture", "name":"fixture", "api":"faux", "provider":"faux",
        "baseUrl":"", "reasoning":false, "input":[], "contextWindow":128000, "maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
    }))
    .expect("model");
    let session = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()),
        agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        model: Some(model),
        session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
        tools: Some(vec!["ls".to_owned()]),
        ..Default::default()
    })
    .await
    .expect("session")
    .session;
    let dispatched: Arc<Mutex<Vec<ToolDispatchedInfo>>> = Arc::default();
    let sink = dispatched.clone();
    let off = session
        .tool_dispatcher()
        .on_dispatched(Arc::new(move |info| {
            sink.lock().expect("observations").push(info.clone())
        }));
    // When: the built-in executes through the session.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.execute_tool("ls", serde_json::json!({}), Default::default()),
    )
    .await
    .expect("bounded built-in execution");
    off();
    session.dispose().await;
    // Then: the call succeeded and the observer saw it dispatched on the server.
    let result = result.expect("built-in ls result");
    assert_ne!(result.is_error, Some(true));
    let dispatched = dispatched.lock().expect("observations");
    assert_eq!(dispatched.len(), 1, "one dispatch per built-in call");
    assert_eq!(dispatched[0].placement, ToolPlacement::Server);
    assert!(!dispatched[0].tool_call_id.is_empty());
}
