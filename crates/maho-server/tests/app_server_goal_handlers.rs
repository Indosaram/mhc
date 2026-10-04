use maho_server::app_server::{envelope::classify_incoming,runtime::AppServerRuntime,thread_registry::SessionFactory};
use serde_json::{Value,json};
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

struct Harness { runtime: AppServerRuntime, receive: tokio::sync::mpsc::UnboundedReceiver<Value> }
async fn harness() -> Harness {
    let directory = tempfile::tempdir().unwrap();
    let runtime = AppServerRuntime::new(directory.path().join("agent").display().to_string(),directory.path().display().to_string(),"1".into(),Some(directory.path().join("sessions").display().to_string()),Some(factory())).await;
    let (send,receive) = tokio::sync::mpsc::unbounded_channel();
    runtime.core.write().await.add_connection("qa".into(),Arc::new(move |message| {send.send(message).unwrap();Box::pin(async {Ok(())})}));
    runtime.core.read().await.receive("qa",classify_incoming(json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"qa","version":"1"}}}))).await.unwrap();
    Harness { runtime, receive }
}
impl Harness {
    async fn call(&mut self,request: Value) -> Value {
        self.runtime.core.read().await.receive("qa",classify_incoming(request)).await.unwrap();
        self.receive.try_recv().unwrap()
    }
    async fn start_thread(&mut self) -> String {
        self.call(json!({"id":2,"method":"thread/start","params":{}})).await["result"]["thread"]["id"].as_str().unwrap().to_owned()
    }
}

#[tokio::test]
async fn goal_set_get_clear_roundtrip_and_response_precedes_notification() {
    let mut harness = harness().await;
    let thread = harness.start_thread().await;
    let response = harness.call(json!({"id":3,"method":"thread/goal/set","params":{"threadId":thread,"objective":"Ship it"}})).await;
    assert_eq!(response["id"],3);
    assert_eq!(response["result"]["goal"]["objective"],"Ship it");
    assert_eq!(response["result"]["goal"]["status"],"active");
    assert_eq!(response["result"]["goal"]["tokenBudget"],Value::Null);
    let notification = tokio::time::timeout(std::time::Duration::from_secs(2),harness.receive.recv()).await.unwrap().unwrap();
    assert_eq!(notification["method"],"thread/goal/updated");
    assert_eq!(notification["params"]["turnId"],Value::Null);
    assert_eq!(notification["params"]["goal"]["objective"],"Ship it");
    let read = harness.call(json!({"id":4,"method":"thread/goal/get","params":{"threadId":thread}})).await;
    assert_eq!(read["result"]["goal"]["objective"],"Ship it");
    let cleared = harness.call(json!({"id":5,"method":"thread/goal/clear","params":{"threadId":thread}})).await;
    assert_eq!(cleared["result"]["cleared"],true);
    let notification = tokio::time::timeout(std::time::Duration::from_secs(2),harness.receive.recv()).await.unwrap().unwrap();
    assert_eq!(notification["method"],"thread/goal/cleared");
    let empty = harness.call(json!({"id":6,"method":"thread/goal/get","params":{"threadId":thread}})).await;
    assert_eq!(empty["result"]["goal"],Value::Null);
    assert_eq!(harness.call(json!({"id":7,"method":"thread/goal/clear","params":{"threadId":thread}})).await["result"]["cleared"],false);
    harness.runtime.dispose().await;
}

#[tokio::test]
async fn goal_set_updates_status_and_budget_and_rejects_unknown_thread_without_state_change() {
    let mut harness = harness().await;
    let thread = harness.start_thread().await;
    harness.call(json!({"id":3,"method":"thread/goal/set","params":{"threadId":thread,"objective":"Tracked","tokenBudget":5000}})).await;
    let paused = harness.call(json!({"id":4,"method":"thread/goal/set","params":{"threadId":thread,"status":"paused"}})).await;
    assert_eq!(paused["result"]["goal"]["status"],"paused");
    assert_eq!(paused["result"]["goal"]["tokenBudget"],json!(5000.0));
    assert_eq!(harness.call(json!({"id":5,"method":"thread/goal/set","params":{"threadId":"missing","objective":"Nope"}})).await["error"]["code"],-32600);
    assert_eq!(harness.call(json!({"id":6,"method":"thread/goal/set","params":{"threadId":thread,"objective":"Fresh"}})).await["error"]["code"],-32600);
    assert_eq!(harness.call(json!({"id":7,"method":"thread/goal/set","params":{"threadId":thread,"status":"bogus"}})).await["error"]["code"],-32602);
    assert_eq!(harness.call(json!({"id":8,"method":"thread/goal/set","params":{"threadId":thread,"status":"blocked"}})).await["error"]["code"],-32600);
    let unchanged = harness.call(json!({"id":9,"method":"thread/goal/get","params":{"threadId":thread}})).await;
    assert_eq!(unchanged["result"]["goal"]["status"],"paused");
    assert_eq!(unchanged["result"]["goal"]["objective"],"Tracked");
    harness.runtime.dispose().await;
}

#[tokio::test]
async fn invalid_status_transition_leaves_persisted_goal_unchanged() {
    let mut harness = harness().await;
    let thread = harness.start_thread().await;
    harness.call(json!({"id":3,"method":"thread/goal/set","params":{"threadId":thread,"objective":"Finish"}})).await;
    let completed = harness.call(json!({"id":4,"method":"thread/goal/set","params":{"threadId":thread,"status":"complete"}})).await;
    assert_eq!(completed["result"]["goal"]["status"],"complete");
    let rejected = harness.call(json!({"id":5,"method":"thread/goal/set","params":{"threadId":thread,"status":"paused"}})).await;
    assert_eq!(rejected["error"]["code"],-32603);
    let read = harness.call(json!({"id":6,"method":"thread/goal/get","params":{"threadId":thread}})).await;
    assert_eq!(read["result"]["goal"]["status"],"complete");
    assert_eq!(read["result"]["goal"]["objective"],"Finish");
    harness.runtime.dispose().await;
}
