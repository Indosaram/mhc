use maho_server::app_server::{envelope::classify_incoming,runtime::AppServerRuntime,thread_registry::SessionFactory};
use serde_json::{Value,json};
use std::collections::VecDeque;
use std::sync::Arc;

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
        let credentials=Arc::new(maho_core::auth_storage::AuthStorage::in_memory([("faux".into(),json!({"type":"api_key","key":"faux-test"}))].into_iter().collect()));
        let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {credentials:Some(credentials),models_path:Some(std::path::Path::new(&cwd).join("models.json")),auth_path:Some(std::path::Path::new(&cwd).join("auth.json")),providers:Some(vec![provider.provider.clone()])});
        AgentSession::new(AgentSessionConfig {
            agent:maho_agent::Agent::new(maho_agent::AgentOptions {initial_state:Some(maho_agent::agent::PartialAgentState {model:Some(model),..Default::default()}),stream_fn:Some(stream_fn),..Default::default()}),
            session_manager:options.session_manager.ok_or("Missing session manager")?,settings_manager:maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),false),
            cwd:cwd.clone(),agent_dir:Some(cwd),fallback_now:Some(Arc::new(||0.0)),retry_random:Some(Arc::new(||0.5)),scoped_models:Vec::new(),favorite_models:Vec::new(),flag_values:Default::default(),custom_tools:Vec::new(),model_runtime:Some(runtime),model_registry:None,uses_default_stream_function:Some(false),initial_active_tool_names:None,default_tool_names:None,eval_only_tool_names:None,allowed_tool_names:None,excluded_tool_names:None,base_tools_override:None,session_start_event:None,auto_title_sessions:Some(false),
        }).map_err(|error|error.to_string())
    }))
}

struct Harness {
    runtime: AppServerRuntime,
    receive: tokio::sync::mpsc::UnboundedReceiver<Value>,
    notifications: VecDeque<Value>,
    _directory: tempfile::TempDir,
}
impl Harness {
    async fn call(&mut self, request: Value) -> Value {
        let id = request["id"].clone();
        self.runtime.core.read().await.receive("qa", classify_incoming(request)).await.unwrap();
        loop {
            let message = tokio::time::timeout(std::time::Duration::from_secs(5), self.receive.recv()).await.expect("response within deadline").expect("connection stays open");
            if message.get("id").is_some_and(|value| *value == id) { return message; }
            self.notifications.push_back(message);
        }
    }
    async fn notification(&mut self) -> Value {
        if let Some(notification) = self.notifications.pop_front() { return notification; }
        tokio::time::timeout(std::time::Duration::from_secs(5), self.receive.recv()).await.expect("notification within deadline").expect("connection stays open")
    }
    fn recorded_before_response(&self) -> usize { self.notifications.len() }
    async fn start_thread(&mut self) -> String {
        let response = self.call(json!({"id":2,"method":"thread/start","params":{}})).await;
        assert_eq!(self.recorded_before_response(),0,"response must precede thread/started");
        assert_eq!(self.notification().await["method"],"thread/started");
        response["result"]["thread"]["id"].as_str().unwrap().to_owned()
    }
    async fn call_notifying(&mut self, request: Value, method: &str) -> (Value, Value) {
        let response = self.call(request).await;
        assert_eq!(self.recorded_before_response(),0,"response must precede {method}");
        let notification = self.notification().await;
        assert_eq!(notification["method"],method);
        (response, notification)
    }
}
async fn harness() -> Harness {
    let directory = tempfile::tempdir().unwrap();
    let runtime = AppServerRuntime::new(directory.path().join("agent").display().to_string(),directory.path().display().to_string(),"1".into(),Some(directory.path().join("sessions").display().to_string()),Some(factory())).await;
    let (send,receive) = tokio::sync::mpsc::unbounded_channel();
    runtime.core.write().await.add_connection("qa".into(),Arc::new(move |message| {send.send(message).unwrap();Box::pin(async {Ok(())})}));
    runtime.core.read().await.receive("qa",classify_incoming(json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"qa","version":"1"}}}))).await.unwrap();
    Harness { runtime, receive, notifications:VecDeque::new(), _directory:directory }
}

#[tokio::test]
async fn goal_set_get_clear_roundtrip_and_response_precedes_notification() {
    let mut harness = harness().await;
    let thread = harness.start_thread().await;
    let (response, notification) = harness.call_notifying(json!({"id":3,"method":"thread/goal/set","params":{"threadId":thread,"objective":"Ship it"}}),"thread/goal/updated").await;
    assert_eq!(response["id"],3);
    assert_eq!(response["result"]["goal"]["objective"],"Ship it");
    assert_eq!(response["result"]["goal"]["status"],"active");
    assert_eq!(response["result"]["goal"]["tokenBudget"],Value::Null);
    assert_eq!(notification["params"]["turnId"],Value::Null);
    assert_eq!(notification["params"]["goal"]["objective"],"Ship it");
    let read = harness.call(json!({"id":4,"method":"thread/goal/get","params":{"threadId":thread}})).await;
    assert_eq!(read["result"]["goal"]["objective"],"Ship it");
    let (cleared, _) = harness.call_notifying(json!({"id":5,"method":"thread/goal/clear","params":{"threadId":thread}}),"thread/goal/cleared").await;
    assert_eq!(cleared["result"]["cleared"],true);
    let empty = harness.call(json!({"id":6,"method":"thread/goal/get","params":{"threadId":thread}})).await;
    assert_eq!(empty["result"]["goal"],Value::Null);
    assert_eq!(harness.call(json!({"id":7,"method":"thread/goal/clear","params":{"threadId":thread}})).await["result"]["cleared"],false);
    assert_eq!(harness.recorded_before_response(),0);
    harness.runtime.dispose().await;
}

#[tokio::test]
async fn goal_set_updates_status_and_budget_and_rejects_unknown_thread_without_state_change() {
    let mut harness = harness().await;
    let thread = harness.start_thread().await;
    harness.call_notifying(json!({"id":3,"method":"thread/goal/set","params":{"threadId":thread,"objective":"Tracked","tokenBudget":5000}}),"thread/goal/updated").await;
    let paused = harness.call_notifying(json!({"id":4,"method":"thread/goal/set","params":{"threadId":thread,"status":"paused"}}),"thread/goal/updated").await.0;
    assert_eq!(paused["result"]["goal"]["status"],"paused");
    assert_eq!(paused["result"]["goal"]["tokenBudget"],json!(5000.0));
    assert_eq!(harness.call(json!({"id":5,"method":"thread/goal/set","params":{"threadId":"missing","objective":"Nope"}})).await["error"]["code"],-32600);
    assert_eq!(harness.call(json!({"id":6,"method":"thread/goal/set","params":{"threadId":thread,"objective":"Fresh"}})).await["error"]["code"],-32600);
    assert_eq!(harness.call(json!({"id":7,"method":"thread/goal/set","params":{"threadId":thread,"status":"bogus"}})).await["error"]["code"],-32602);
    assert_eq!(harness.call(json!({"id":8,"method":"thread/goal/set","params":{"threadId":thread,"status":"blocked"}})).await["error"]["code"],-32600);
    assert_eq!(harness.recorded_before_response(),0);
    let unchanged = harness.call(json!({"id":9,"method":"thread/goal/get","params":{"threadId":thread}})).await;
    assert_eq!(unchanged["result"]["goal"]["status"],"paused");
    assert_eq!(unchanged["result"]["goal"]["objective"],"Tracked");
    harness.runtime.dispose().await;
}

#[tokio::test]
async fn invalid_status_transition_leaves_persisted_goal_unchanged() {
    let mut harness = harness().await;
    let thread = harness.start_thread().await;
    harness.call_notifying(json!({"id":3,"method":"thread/goal/set","params":{"threadId":thread,"objective":"Finish"}}),"thread/goal/updated").await;
    let completed = harness.call_notifying(json!({"id":4,"method":"thread/goal/set","params":{"threadId":thread,"status":"complete"}}),"thread/goal/updated").await.0;
    assert_eq!(completed["result"]["goal"]["status"],"complete");
    let rejected = harness.call(json!({"id":5,"method":"thread/goal/set","params":{"threadId":thread,"status":"paused"}})).await;
    assert_eq!(rejected["error"]["code"],-32603);
    assert_eq!(harness.recorded_before_response(),0);
    let read = harness.call(json!({"id":6,"method":"thread/goal/get","params":{"threadId":thread}})).await;
    assert_eq!(read["result"]["goal"]["status"],"complete");
    assert_eq!(read["result"]["goal"]["objective"],"Finish");
    harness.runtime.dispose().await;
}

/// UB1 causal regression: the runtime registers a per-thread MCP wire-status inventory holder on
/// `thread/start` (post-response, via `bind_mcp_wire_status` -> `register_thread`), observable through
/// the registry completion signal taken BEFORE the trigger (no sleep/poll).
#[tokio::test]
async fn runtime_registers_a_per_thread_mcp_inventory_on_thread_start() {
    let mut harness = harness().await;
    let registration = harness.runtime.mcp_inventory.lock().await.ready_signal();
    let thread = harness.start_thread().await;
    tokio::time::timeout(std::time::Duration::from_secs(5), registration.notified())
        .await
        .expect("per-thread MCP inventory registration within deadline");
    let inventory = harness.runtime.mcp_inventory.lock().await;
    assert!(inventory.registration_count() >= 1, "the runtime must register a per-thread MCP inventory holder on thread start");
    assert!(inventory.resolve(Some(&thread)).is_some(), "the started thread must resolve to its registered holder");
    assert!(inventory.resolve(None).is_some(), "the process-global holder remains available");
    drop(inventory);
    harness.runtime.dispose().await;
}
