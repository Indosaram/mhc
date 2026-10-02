use futures_util::{SinkExt, StreamExt};
use maho_server::app_server::{runtime::AppServerRuntime, thread_registry::SessionFactory, unix_socket::start_unix_socket_listener, websocket_auth::ResolvedWebSocketListenerAuth};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_tungstenite::{WebSocketStream, client_async, tungstenite::Message};

type Client = WebSocketStream<tokio::net::UnixStream>;
async fn send(client: &mut Client, value: Value) {
    client.send(Message::Text(value.to_string().into())).await.expect("client frame sent");
}
async fn read(client: &mut Client) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        serde_json::from_str(client.next().await.expect("server frame").expect("valid websocket frame").to_text().expect("text frame")).expect("JSON response")
    }).await.expect("response within deadline")
}
async fn attach(path: &std::path::Path) -> Client {
    let (mut client, _) = client_async("ws://localhost/", tokio::net::UnixStream::connect(path).await.expect("Unix listener reachable")).await.expect("websocket handshake");
    send(&mut client,json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"qa","version":"1"}}})).await;
    assert_eq!(read(&mut client).await["id"],1);
    client
}
fn factory() -> SessionFactory {
    Arc::new(|options| Box::pin(async move {
        use maho_ai::providers::faux::{FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_provider, faux_streams};
        use maho_core::agent_session::{AgentSession, AgentSessionConfig};
        let cwd = options.cwd.unwrap_or_default();
        let provider = faux_provider(RegisterFauxProviderOptions {api:Some("faux".into()),models:Some(vec![maho_ai::providers::faux::FauxModelDefinition {id:"faux-1".into(),reasoning:Some(true),..Default::default()}]),tokens_per_second:Some(0.0),..Default::default()});
        let model = provider.get_model(Some("faux-1")).ok_or("Missing faux model")?;
        provider.set_responses(vec![faux_assistant_message("retained transcript",FauxAssistantMessageOptions {timestamp:Some(0),..Default::default()}).into()]);
        let streams = faux_streams(provider.core.clone());
        let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| streams.stream_simple(model,context,options.map(|options|options.simple)));
        let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {models_path:Some(std::path::Path::new(&cwd).join("models.json")),auth_path:Some(std::path::Path::new(&cwd).join("auth.json")),providers:Some(vec![provider.provider.clone()]),..Default::default()});
        AgentSession::new(AgentSessionConfig {
            agent:maho_agent::Agent::new(maho_agent::AgentOptions {initial_state:Some(maho_agent::agent::PartialAgentState {model:Some(model),..Default::default()}),stream_fn:Some(stream_fn),..Default::default()}),
            session_manager:options.session_manager.ok_or("Missing session manager")?,settings_manager:maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),false),
            cwd:cwd.clone(),agent_dir:Some(cwd),fallback_now:Some(Arc::new(||0.0)),retry_random:Some(Arc::new(||0.5)),scoped_models:Vec::new(),favorite_models:Vec::new(),flag_values:Default::default(),custom_tools:Vec::new(),model_runtime:Some(runtime),model_registry:None,uses_default_stream_function:Some(false),initial_active_tool_names:None,default_tool_names:None,eval_only_tool_names:None,allowed_tool_names:None,excluded_tool_names:None,base_tools_override:None,session_start_event:None,auto_title_sessions:Some(false),
        }).map_err(|error|error.to_string())
    }))
}

#[tokio::test]
async fn daemon_socket_faux_turn_detach_and_reattach_retains_transcript() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("app.sock");
    let runtime = AppServerRuntime::new(directory.path().join("agent").display().to_string(),directory.path().display().to_string(),"1".into(),Some(directory.path().join("sessions").display().to_string()),Some(factory())).await;
    let listener = start_unix_socket_listener(path.clone(),true,ResolvedWebSocketListenerAuth::Off,runtime.core.clone(),None).await.unwrap();
    let mut client = attach(&path).await;
    send(&mut client,json!({"id":2,"method":"thread/start","params":{}})).await;
    let response = read(&mut client).await;
    let id = response["result"]["thread"]["id"].as_str().unwrap().to_owned();
    assert_eq!(read(&mut client).await["method"],"thread/started");
    send(&mut client,json!({"id":3,"method":"turn/start","params":{"threadId":id,"input":[{"type":"text","text":"hello"}]}})).await;
    let accepted = read(&mut client).await;
    assert_eq!(accepted["id"],3);
    let completed = loop {
        let message = read(&mut client).await;
        if message["method"] == "turn/completed" { break message; }
    };
    assert_eq!(completed["params"]["turn"]["status"],"completed");
    let items = completed["params"]["turn"]["items"].clone();
    assert!(items.as_array().unwrap().iter().any(|item|item["text"] == "retained transcript"));
    send(&mut client,json!({"id":4,"method":"thread/unsubscribe","params":{"threadId":id}})).await;
    assert_eq!(read(&mut client).await["id"],4);
    client.close(None).await.unwrap(); drop(client);
    let mut client = attach(&path).await;
    send(&mut client,json!({"id":5,"method":"thread/resume","params":{"threadId":id}})).await;
    let resumed = read(&mut client).await;
    assert_eq!(resumed["result"]["thread"]["turns"][0]["items"],items);
    assert_eq!(read(&mut client).await["method"],"thread/status/changed");
    send(&mut client,json!({"id":6,"method":"thread/turns/list","params":{"threadId":id,"itemsView":"full"}})).await;
    let history = read(&mut client).await;
    assert_eq!(history["result"]["data"][0]["items"],items);
    send(&mut client,json!({"id":7,"method":"thread/items/list","params":{"threadId":id,"limit":1}})).await;
    let page = read(&mut client).await;
    assert_eq!(page["result"]["data"].as_array().unwrap().len(),1);
    assert_eq!(page["result"]["data"][0]["item"]["type"],"userMessage");
    let cursor = page["result"]["nextCursor"].as_str().unwrap();
    send(&mut client,json!({"id":8,"method":"thread/items/list","params":{"threadId":id,"cursor":cursor}})).await;
    let next = read(&mut client).await;
    assert_eq!(next["result"]["data"][0]["item"]["text"],"retained transcript");
    send(&mut client,json!({"id":9,"method":"thread/search","params":{"searchTerm":"hello","sourceKinds":["appServer"]}})).await;
    let search = read(&mut client).await;
    assert_eq!(search["result"]["data"][0]["thread"]["id"],id,"{search}");
    assert!(search["result"]["data"][0]["snippet"].as_str().unwrap().contains("hello"));
    client.close(None).await.unwrap(); drop(client);
    listener.close().await.unwrap();
    runtime.dispose().await;
    assert!(!path.exists());
}

#[tokio::test]
async fn thread_settings_validate_before_mutation_and_respond_before_notification() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.sock");
    let runtime = AppServerRuntime::new(directory.path().display().to_string(),directory.path().display().to_string(),"1".into(),Some(directory.path().join("sessions").display().to_string()),Some(factory())).await;
    let listener = start_unix_socket_listener(path.clone(),true,ResolvedWebSocketListenerAuth::Off,runtime.core.clone(),None).await.unwrap();
    let (mut client,_) = client_async("ws://localhost/",tokio::net::UnixStream::connect(&path).await.unwrap()).await.unwrap();
    send(&mut client,json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"qa","version":"1"},"capabilities":{"experimentalApi":true}}})).await;
    assert_eq!(read(&mut client).await["id"],1);
    send(&mut client,json!({"id":2,"method":"thread/start","params":{}})).await;
    let response = read(&mut client).await;
    let id = response["result"]["thread"]["id"].as_str().unwrap().to_owned();
    assert_eq!(read(&mut client).await["method"],"thread/started");
    send(&mut client,json!({"id":3,"method":"thread/settings/update","params":{"threadId":id,"effort":"unrecognized"}})).await;
    assert_eq!(read(&mut client).await["error"]["code"],-32603);
    let entry = runtime.threads.get_loaded_thread(&id).await.unwrap();
    assert_eq!(serde_json::to_value(entry.lock().await.session.thinking_level()).unwrap(),"off");
    send(&mut client,json!({"id":4,"method":"thread/settings/update","params":{"threadId":id,"effort":"minimal"}})).await;
    let response = read(&mut client).await;
    assert_eq!(response["id"],4);
    assert_eq!(response["result"],json!({}));
    let notification = read(&mut client).await;
    assert_eq!(notification["method"],"thread/settings/updated");
    assert_eq!(notification["params"]["threadSettings"]["effort"],"minimal");
    send(&mut client,json!({"id":5,"method":"thread/metadata/update","params":{"threadId":id,"gitInfo":{"branch":"feature"}}})).await;
    let metadata = read(&mut client).await;
    assert_eq!(metadata["result"]["thread"]["gitInfo"]["branch"],"feature");
    send(&mut client,json!({"id":6,"method":"thread/metadata/update","params":{"threadId":id,"gitInfo":{}}})).await;
    assert_eq!(read(&mut client).await["error"]["code"],-32600);
    let listing = runtime.threads.list_threads(None,1).await;
    assert_eq!(listing["threads"][0]["id"],id);
    assert!(listing["nextCursor"].is_null());
    send(&mut client,json!({"id":7,"method":"thread/list","params":{}})).await;
    let listed = read(&mut client).await;
    assert_eq!(listed["result"]["data"][0]["id"],id,"{listed}");
    assert_eq!(listed["result"]["data"][0]["gitInfo"]["branch"],"feature");
    send(&mut client,json!({"id":8,"method":"thread/loaded/list","params":{"limit":0}})).await;
    let loaded = read(&mut client).await;
    assert!(loaded["result"]["data"].as_array().unwrap().is_empty());
    assert!(loaded["result"]["nextCursor"].is_string());
    let search_root = directory.path().join("search-root");
    std::fs::create_dir(&search_root).unwrap();
    std::fs::write(search_root.join("needle.txt"),"test").unwrap();
    send(&mut client,json!({"id":9,"method":"fuzzyFileSearch","params":{"query":"needle","roots":[search_root.display().to_string()]}})).await;
    let found = read(&mut client).await;
    assert_eq!(found["id"],9);
    assert_eq!(found["result"]["files"].as_array().unwrap().len(),1);
    client.close(None).await.unwrap();drop(client);
    listener.close().await.unwrap();runtime.dispose().await;
    assert!(!path.exists());
}
