use maho_core::{
    model_runtime::{CreateModelRuntimeOptions, ModelRuntime},
    sdk::{create_agent_session, CreateAgentSessionOptions, NoToolsMode},
    session_manager::SessionManager,
    settings_manager::{InMemorySettingsStorage, SettingsManager},
};
use maho_ai::providers::faux::faux_provider;
use maho_ext_host::loader::NativeAsyncExtensionFactory;
use maho_rpc::connection_handler::RpcConnectionHandler;
use maho_ext_api::mcp_wire::{
    McpWireAuthStatus, McpWireStatusServer, McpWireStatusSnapshot,
    MCP_CONTROL_INVENTORY_REQUEST_EVENT,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc;

fn snapshot(name: &str) -> McpWireStatusSnapshot {
    McpWireStatusSnapshot {
        servers: vec![McpWireStatusServer {
            name: name.into(),
            server_info: None,
            tools: Vec::new(),
            resources: Vec::new(),
            resource_templates: Vec::new(),
            auth_status: McpWireAuthStatus::NotLoggedIn,
            status: None,
        }],
    }
}

async fn session(root: &std::path::Path) -> maho_core::agent_session::AgentSession {
    let cwd = root.to_path_buf();
    let agent_dir = cwd.join("agent");
    std::fs::create_dir_all(&agent_dir).expect("agent directory");
    let cwd_string = cwd.to_string_lossy().into_owned();
    let agent_dir_string = agent_dir.to_string_lossy().into_owned();
    let provider = faux_provider(Default::default());
    let model = provider.get_model(Some("faux-1")).expect("faux model");
    let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
        providers: Some(vec![provider.provider]),
        ..Default::default()
    });
    let inventory_factory = NativeAsyncExtensionFactory {
        path: "<test:loaded-surfaces-inventory>".into(),
        source_info: Default::default(),
        factory: Arc::new(|api| {
            Box::pin(async move {
                let initial = snapshot("fixture-mcp");
                let request_subscription = api.events.on_native::<
                    maho_ext_api::mcp_wire::McpControlInventoryRequest,
                >(
                    MCP_CONTROL_INVENTORY_REQUEST_EVENT,
                    Arc::new(move |request| {
                        if let Some(respond) = request
                            .respond
                            .lock()
                            .expect("inventory response mutex")
                            .take()
                        {
                            let _ = respond.send(
                                serde_json::to_value(&initial).expect("serialize initial snapshot"),
                            );
                        }
                    }),
                );
                let request_subscription = Arc::new(std::sync::Mutex::new(Some(request_subscription)));
                api.on(
                    maho_ext_api::EventKind::SessionShutdown,
                    Arc::new(move |_, _| {
                        request_subscription.lock().expect("inventory subscription").take();
                        Box::pin(async { Ok(maho_ext_api::EventResult::None) })
                    }),
                );
                Ok(())
            })
        }),
    };
    create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd_string.clone()),
        agent_dir: Some(agent_dir_string),
        model_runtime: Some(runtime),
        model: Some(model),
        session_manager: Some(SessionManager::in_memory(&cwd_string, None, None)),
        settings_manager: Some(SettingsManager::from_storage(
            Box::new(InMemorySettingsStorage::default()),
            false,
        )),
        extension_factories: vec![inventory_factory],
        no_tools: Some(NoToolsMode::All),
        auto_title_sessions: Some(false),
        ..Default::default()
    })
    .await
    .expect("create native agent session")
    .session
}

async fn next_record(receive: &mut mpsc::UnboundedReceiver<Value>) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(5), receive.recv())
        .await
        .expect("RPC record arrived before timeout")
        .expect("RPC output channel remains open")
}

#[tokio::test]
async fn request_projects_fresh_native_inventory() {
    let root = tempfile::tempdir().expect("temporary cwd");
    let session = session(root.path()).await;
    let response = maho_rpc::connection_handler::handle_input_line(
        &session,
        &json!({"type":"get_loaded_surfaces"}).to_string(),
    )
    .await
    .expect("RPC dispatch")
    .expect("RPC response");
    let response: Value = serde_json::from_str(&response).expect("JSON response");
    assert_eq!(response["success"], true);
    assert_eq!(
        response["data"]["mcpServers"],
        json!([{"name":"fixture-mcp","toolCount":0,"status":"needs_auth","authStatus":"notLoggedIn"}])
    );
    assert_eq!(response["data"]["extensions"].as_array().map(Vec::len), Some(1));
    session.dispose().await;
}

#[tokio::test]
async fn live_changed_snapshot_emits_one_change_and_dispose_releases_subscription() {
    let root = tempfile::tempdir().expect("temporary cwd");
    let session = session(root.path()).await;
    let (output, mut receive) = mpsc::unbounded_channel::<Value>();
    let handler = Box::pin(RpcConnectionHandler::create(
        session.clone(),
        Arc::new(move |record| {
            let _ = output.send(record);
        }),
        Vec::new(),
    ))
    .await;

    let (baseline_signal, mut baseline_receive) = mpsc::unbounded_channel();
    let baseline_subscription = session
        .bind_mcp_wire_status::<McpWireStatusSnapshot>(Arc::new(move |current| {
            let _ = baseline_signal.send(current.clone());
        }))
        .await
        .expect("bind baseline snapshot observer");
    let baseline = snapshot("fixture-mcp");
    assert!(session.publish_mcp_wire_status(&baseline).await);
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            baseline_receive.recv(),
        )
        .await
        .expect("baseline snapshot publication arrives")
        .expect("baseline observer remains open"),
        baseline
    );
    drop(baseline_subscription);

    let changed = snapshot("changed-mcp");
    assert!(session.publish_mcp_wire_status(&changed).await);
    let change = next_record(&mut receive).await;
    assert_eq!(change["type"], "loaded_surfaces_changed");
    assert_eq!(change, json!({"type":"loaded_surfaces_changed"}));

    assert!(session.publish_mcp_wire_status(&changed).await);
    let fence = snapshot("fence-mcp");
    assert!(session.publish_mcp_wire_status(&fence).await);
    let fence_event = next_record(&mut receive).await;
    assert_eq!(fence_event["type"], "loaded_surfaces_changed");
    assert_eq!(fence_event, json!({"type":"loaded_surfaces_changed"}));
    assert!(receive.try_recv().is_err(), "unchanged snapshot emits no duplicate event");

    Box::pin(handler.dispose()).await;
    assert!(session.bind_mcp_wire_status::<McpWireStatusSnapshot>(Arc::new(|_| {})).await.is_none());
    assert!(receive.try_recv().is_err(), "disposed handler emits no inventory event");
}
