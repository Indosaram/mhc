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
        provider.set_responses(vec![faux_assistant_message("forked",FauxAssistantMessageOptions {timestamp:Some(0),..Default::default()}).into()]);
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

struct Harness { runtime: AppServerRuntime, receive: tokio::sync::mpsc::UnboundedReceiver<Value>, session_dir: std::path::PathBuf }
async fn harness() -> Harness {
    let directory = tempfile::tempdir().unwrap();
    let session_dir = directory.path().join("sessions");
    std::fs::create_dir_all(&session_dir).unwrap();
    let runtime = AppServerRuntime::new(directory.path().join("agent").display().to_string(),directory.path().display().to_string(),"1".into(),Some(session_dir.display().to_string()),Some(factory())).await;
    let (send,receive) = tokio::sync::mpsc::unbounded_channel();
    runtime.core.write().await.add_connection("qa".into(),Arc::new(move |message| {send.send(message).unwrap();Box::pin(async {Ok(())})}));
    runtime.core.read().await.receive("qa",classify_incoming(json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"qa","version":"1"}}})).await.unwrap();
    Harness { runtime, receive, session_dir }
}
impl Harness {
    async fn call(&mut self,request: Value) -> Value {
        self.runtime.core.read().await.receive("qa",classify_incoming(request)).await.unwrap();
        self.receive.try_recv().unwrap()
    }
    fn write_source(&self,id: &str) -> std::path::PathBuf {
        let path = self.session_dir.join(format!("2020-01-01T00-00-00-000Z_{id}.jsonl"));        let records = [json!({"type":"session","id":id,"version":3,"cwd":"/work","timestamp":"2020-01-01T00:00:00.000Z"}),json!({"type":"message","id":"u","parentId":null,"timestamp":"2020-01-01T00:00:01.000Z","message":{"role":"user","content":"hello"}}),json!({"type":"message","id":"a","parentId":"u","timestamp":"2020-01-01T00:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"persisted"}]}})];
        std::fs::write(&path,records.map(|record|record.to_string()).join("\n")).unwrap();
        path
    }
}

#[tokio::test]
async fn fork_copies_history_into_a_new_persisted_session_referencing_the_source() {
    let mut harness = harness().await;
    harness.write_source("source-thread");
    let response = harness.call(json!({"id":2,"method":"thread/fork","params":{"threadId":"source-thread"}})).await;
    assert_eq!(response["id"],2);
    assert_eq!(response["result"]["thread"]["forkedFromId"],"source-thread");
    let forked_id = response["result"]["thread"]["id"].as_str().unwrap().to_owned();
    assert_ne!(forked_id,"source-thread");
    assert_eq!(response["result"]["thread"]["turns"].as_array().unwrap().len(),1);
    let notification = tokio::time::timeout(std::time::Duration::from_secs(2),harness.receive.recv()).await.unwrap().unwrap();
    assert_eq!(notification["method"],"thread/started");
    assert_eq!(notification["params"]["thread"]["id"],forked_id);
    let forked_path = response["result"]["thread"]["path"].as_str().unwrap();
    let contents = std::fs::read_to_string(forked_path).unwrap();
    let header: Value = serde_json::from_str(contents.lines().next().unwrap()).unwrap();
    assert_eq!(header["type"],"session");
    assert_eq!(header["id"],forked_id);
    assert!(header["parentSession"].as_str().unwrap().ends_with("source-thread.jsonl"));
    assert!(contents.lines().any(|line| line.contains("\"persisted\"")));
    assert_eq!(harness.call(json!({"id":3,"method":"thread/fork","params":{"threadId":"missing"}})).await["error"]["code"],-32603);
    harness.runtime.dispose().await;
}
