use maho_server::app_server::{envelope::classify_incoming,runtime::AppServerRuntime,thread_registry::SessionFactory};
use serde_json::{Value,json};
use std::sync::Arc;

fn factory(settings: &'static str) -> SessionFactory {
    Arc::new(move |options| Box::pin(async move {
        use maho_ai::providers::faux::{FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_provider, faux_streams};
        use maho_core::agent_session::{AgentSession, AgentSessionConfig};
        let cwd = options.cwd.unwrap_or_default();
        let provider = faux_provider(RegisterFauxProviderOptions {api:Some("faux".into()),models:Some(vec![maho_ai::providers::faux::FauxModelDefinition {id:"faux-1".into(),reasoning:Some(true),..Default::default()}]),tokens_per_second:Some(0.0),..Default::default()});
        let model = provider.get_model(Some("faux-1")).ok_or("Missing faux model")?;
        provider.set_responses(vec![faux_assistant_message("extension reply",FauxAssistantMessageOptions {timestamp:Some(0),..Default::default()}).into()]);
        let streams = faux_streams(provider.core.clone());
        let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| streams.stream_simple(model,context,options.map(|options|options.simple)));
        let credentials=Arc::new(maho_core::auth_storage::AuthStorage::in_memory([("faux".into(),json!({"type":"api_key","key":"faux-test"}))].into_iter().collect()));
        let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {credentials:Some(credentials),models_path:Some(std::path::Path::new(&cwd).join("models.json")),auth_path:Some(std::path::Path::new(&cwd).join("auth.json")),providers:Some(vec![provider.provider.clone()]), ..Default::default() });
        let storage = maho_core::settings_manager::InMemorySettingsStorage::with_global_content(settings);
        AgentSession::new(AgentSessionConfig {
            agent:maho_agent::Agent::new(maho_agent::AgentOptions {initial_state:Some(maho_agent::agent::PartialAgentState {model:Some(model),..Default::default()}),stream_fn:Some(stream_fn),..Default::default()}),
            session_manager:options.session_manager.ok_or("Missing session manager")?,settings_manager:maho_core::settings_manager::SettingsManager::from_storage(Box::new(storage),false),
            cwd:cwd.clone(),agent_dir:Some(cwd),fallback_now:Some(Arc::new(||0.0)),retry_random:Some(Arc::new(||0.5)),scoped_models:Vec::new(),favorite_models:Vec::new(),flag_values:Default::default(),custom_tools:Vec::new(),model_runtime:Some(runtime),model_registry:None,uses_default_stream_function:Some(false),initial_active_tool_names:None,default_tool_names:None,eval_only_tool_names:None,allowed_tool_names:None,excluded_tool_names:None,base_tools_override:None,session_start_event:None,auto_title_sessions:Some(false),
        }).map_err(|error|error.to_string())
    }))
}

struct Harness {
    runtime: AppServerRuntime,
    receive: tokio::sync::mpsc::UnboundedReceiver<Value>,
    _directory: tempfile::TempDir,
}
impl Harness {
    async fn new(settings: &'static str) -> Self {
        let directory = tempfile::tempdir().expect("temp dir");
        let runtime = AppServerRuntime::new(directory.path().join("agent").display().to_string(),directory.path().display().to_string(),"1".into(),Some(directory.path().join("sessions").display().to_string()),Some(factory(settings))).await;
        let (send,receive) = tokio::sync::mpsc::unbounded_channel();
        runtime.core.write().await.add_connection("qa".into(),Arc::new(move |message| {send.send(message).expect("connection stays open");Box::pin(async {Ok(())})}));
        let mut harness = Self { runtime, receive, _directory:directory };
        harness.call(json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"qa","version":"1"}}})).await;
        harness
    }
    async fn next(&mut self) -> Value {
        tokio::time::timeout(std::time::Duration::from_secs(10), self.receive.recv()).await.expect("message within deadline").expect("connection stays open")
    }
    async fn call(&mut self, request: Value) -> Value {
        let id = request["id"].clone();
        self.runtime.core.read().await.receive("qa", classify_incoming(request)).await.expect("request dispatch succeeds");
        loop {
            let message = self.next().await;
            if message.get("id").is_some_and(|value| *value == id) { return message; }
        }
    }
    async fn until(&mut self, method: &str) -> Vec<Value> {
        let mut seen = Vec::new();
        loop {
            let message = self.next().await;
            let done = message["method"] == method;
            seen.push(message);
            if done { return seen; }
        }
    }
    async fn start_thread(&mut self) -> String {
        let response = self.call(json!({"id":2,"method":"thread/start","params":{}})).await;
        self.until("thread/started").await;
        response["result"]["thread"]["id"].as_str().expect("thread id present").to_owned()
    }
}
fn methods(messages: &[Value]) -> Vec<&str> { messages.iter().filter_map(|message| message["method"].as_str()).collect() }

#[tokio::test]
async fn a_turn_the_client_did_not_start_is_projected_and_completed() {
    let mut harness = Harness::new("{}").await;
    let thread = harness.start_thread().await;
    let session = harness.runtime.threads.get_loaded_thread(&thread).await.expect("thread loaded").lock().await.session.clone();
    let prompt = tokio::spawn(async move { session.prompt("wake up", maho_core::agent_session::PromptOptions::default()).await });
    let messages = harness.until("turn/completed").await;
    prompt.await.expect("prompt task joins").expect("prompt succeeds");
    let methods = methods(&messages);
    let started = methods.iter().position(|method| *method == "turn/started").expect("turn/started emitted");
    assert!(methods[..started].contains(&"thread/status/changed"), "{methods:?}");
    let reply = messages.iter().find(|message| message["method"] == "item/completed" && message["params"]["item"]["text"] == "extension reply").expect("assistant item projected");
    assert!(reply["params"]["completedAtMs"].is_number());
    let completed = messages.last().expect("turn/completed");
    assert_eq!(completed["params"]["turn"]["status"], "completed");
    assert_eq!(completed["params"]["turn"]["id"], messages[started]["params"]["turn"]["id"]);
    harness.runtime.dispose().await;
}

#[tokio::test]
async fn a_prompt_rejected_before_admission_fails_turn_start() {
    let mut harness = Harness::new(r#"{"compaction":{"reserveTokens":"invalid"}}"#).await;
    let thread = harness.start_thread().await;
    harness.runtime.core.read().await.receive("qa", classify_incoming(json!({"id":3,"method":"turn/start","params":{"threadId":thread,"input":[{"type":"text","text":"hello"}]}}))).await.expect("request dispatch succeeds");
    let mut messages = Vec::new();
    let response = loop {
        let message = harness.next().await;
        if message["id"] == 3 { break message; }
        messages.push(message);
    };
    assert_eq!(response["error"]["code"], -32603, "{response}");
    assert_eq!(methods(&messages), ["thread/status/changed","error","turn/completed"]);
    let completed = messages.last().expect("turn/completed");
    assert_eq!(completed["params"]["turn"]["status"], "failed");
    assert_eq!(completed["params"]["turn"]["error"]["message"], "Prompt preflight failed");
    harness.runtime.dispose().await;
}

#[tokio::test]
async fn a_projected_stream_error_fails_the_turn_with_one_error_notification() {
    use maho_agent::types::{AgentEvent, AgentMessage};
    use maho_ai::types::{AssistantMessageEvent, ErrorReason, Message, StopReason};
    let mut harness = Harness::new("{}").await;
    let thread = harness.start_thread().await;
    let session = harness.runtime.threads.get_loaded_thread(&thread).await.expect("thread loaded").lock().await.session.clone();
    let failure = maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions {stop_reason:Some(StopReason::Error),error_message:Some("provider exploded".into()),..Default::default()});
    session.emit(maho_ext_api::AgentSessionEvent::Agent(AgentEvent::AgentStart));
    session.emit(maho_ext_api::AgentSessionEvent::Agent(AgentEvent::MessageUpdate {message:AgentMessage::from(Message::Assistant(Box::new(failure.clone()))),assistant_message_event:AssistantMessageEvent::Error {reason:ErrorReason::Error,error:failure}}));
    let messages = harness.until("turn/completed").await;
    let errors = messages.iter().filter(|message| message["method"] == "error").collect::<Vec<_>>();
    assert_eq!(errors.len(), 1, "{messages:?}");
    let completed = messages.last().expect("turn/completed");
    assert_eq!(completed["params"]["turn"]["status"], "failed");
    assert_eq!(completed["params"]["turn"]["error"]["message"], "provider exploded");
    assert_eq!(errors[0]["params"]["turnId"], completed["params"]["turn"]["id"]);
    harness.runtime.dispose().await;
}
