use maho_core::{model_runtime::{CreateModelRuntimeOptions, ModelRuntime}, sdk::{CreateAgentSessionOptions, NoToolsMode, create_agent_session}, session_manager::SessionManager, settings_manager::{InMemorySettingsStorage, SettingsManager}};
use maho_rpc::connection_handler::RpcConnectionHandler;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

struct PromptOAuth { started: Arc<AtomicBool> }

#[async_trait::async_trait]
impl maho_ai::auth::types::OAuthAuth for PromptOAuth {
    fn name(&self) -> &str { "RPC Prompt Fixture" }
    async fn login(&self, interaction: &maho_ai::auth::types::ProviderAuthInteraction) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> {
        interaction.notify(maho_ai::auth::types::AuthEvent::AuthUrl { url: "https://auth.fixture.test/start".into(), instructions: None });
        self.started.store(true, Ordering::SeqCst);
        let answer = interaction.prompt(maho_ai::auth::types::AuthPrompt { kind: maho_ai::auth::types::AuthPromptKind::Text { message: "Fixture code".into(), placeholder: Some("code".into()) }, signal: None }).await?;
        interaction.signal.throw_if_aborted()?;
        Ok(maho_ai::auth::types::OAuthCredential::new(answer, "fixture-refresh", 10_000.0))
    }
    async fn refresh(&self, credential: &maho_ai::auth::types::OAuthCredential, _signal: &maho_ai::utils::abort::AbortSignal) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> { Ok(credential.clone()) }
    async fn to_auth(&self, credential: &maho_ai::auth::types::OAuthCredential) -> anyhow::Result<maho_ai::auth::types::ModelAuth> { Ok(maho_ai::auth::types::ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() }) }
}

async fn make_session(root: &std::path::Path) -> maho_core::agent_session::AgentSession {
    let cwd = root.to_string_lossy().into_owned();
    let manager = SessionManager::create(&cwd, Some(root.join("sessions").to_str().expect("directory")), None);
    let provider = maho_ai::providers::faux::faux_provider(Default::default());
    let model = provider.get_model(Some("faux-1")).expect("model");
    let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions { providers: Some(vec![provider.provider]), ..Default::default() });
    create_agent_session(CreateAgentSessionOptions { cwd: Some(cwd.clone()), agent_dir: Some(cwd), model_runtime: Some(runtime), model: Some(model), session_manager: Some(manager), settings_manager: Some(SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), false)), no_tools: Some(NoToolsMode::All), auto_title_sessions: Some(false), ..Default::default() }).await.expect("session").session
}

async fn send(handler: &RpcConnectionHandler, command: Value) -> Value {
    let response = handler.handle_input_line(&command.to_string()).await.expect("dispatch").expect("response");
    serde_json::from_str(&response).expect("JSON response")
}

#[tokio::test]
async fn api_key_login_logout_and_unmatched_cancel_follow_rpc_contract() {
    let temp = tempfile::tempdir().expect("root");
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let session = make_session(temp.path()).await;
    let handler = RpcConnectionHandler::create(session, Arc::new(move |event| { let _ = event_tx.send(event); }), Vec::new()).await;
    let response = send(&handler, json!({"id":"providers","type":"get_auth_providers"})).await;
    assert_eq!(response["success"], true);
    assert!(response["data"]["providers"].is_array());
    let response = send(&handler, json!({"id":"key","type":"login_api_key","provider":"faux","key":"fixture-secret"})).await;
    assert_eq!(response["success"], true);
    let providers = send(&handler, json!({"id":"providers-after-login","type":"get_auth_providers"})).await;
    assert!(providers["data"]["providers"].as_array().expect("providers").iter().any(|provider| provider["id"] == "faux" && provider["status"]["configured"] == true));
    let response = send(&handler, json!({"id":"cancel","type":"login_cancel","provider":"faux"})).await;
    assert_eq!(response["success"], true);
    let response = send(&handler, json!({"id":"logout","type":"logout","provider":"faux"})).await;
    assert_eq!(response["success"], true);
    assert!(event_rx.try_recv().is_err());
    handler.dispose().await;
}

#[tokio::test]
async fn login_cancel_closes_prompt_and_emits_one_terminal_event() {
    let temp = tempfile::tempdir().expect("root");
    let session = make_session(temp.path()).await;
    let started = Arc::new(AtomicBool::new(false));
    session.model_registry().auth_storage.register_oauth_provider("rpc-prompt-fixture", Arc::new(PromptOAuth { started: started.clone() }));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let handler = RpcConnectionHandler::create(session, Arc::new(move |event| { let _ = event_tx.send(event); }), Vec::new()).await;
    let response = send(&handler, json!({"id":"start","type":"login_start","provider":"rpc-prompt-fixture"})).await;
    assert_eq!(response["success"], true);
    let mut auth_url = false;
    let prompt_event = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let event = event_rx.recv().await.expect("RPC event stream");
            if event["type"] == "auth_login_url" { auth_url = event["url"] == "https://auth.fixture.test/start"; }
            if event["method"] == "input" { break event; }
        }
    }).await.expect("auth URL and prompt events");
    assert!(started.load(Ordering::SeqCst), "provider login reached prompt");
    assert!(auth_url);
    let prompt_id = prompt_event["id"].as_str().expect("prompt request id").to_owned();
    let cancel = send(&handler, json!({"id":"cancel","type":"login_cancel","provider":"rpc-prompt-fixture"})).await;
    assert_eq!(cancel["success"], true);
    let mut terminal_count = 0;
    let end = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let event = event_rx.recv().await.expect("RPC event stream");
            if event["type"] == "auth_login_end" { terminal_count += 1; break event; }
        }
    }).await.expect("terminal auth event");
    assert_eq!(end["success"], false);
    assert_eq!(end["provider"], "rpc-prompt-fixture");
    assert_eq!(terminal_count, 1);
    let response = handler.handle_input_line(&json!({"type":"extension_ui_response","id":prompt_id,"cancelled":true}).to_string()).await.expect("prompt cancellation");
    assert!(response.is_none());
    handler.dispose().await;
}

#[tokio::test]
async fn login_start_prompt_response_emits_exact_url_and_success_events() {
    let temp = tempfile::tempdir().expect("root");
    let session = make_session(temp.path()).await;
    let started = Arc::new(AtomicBool::new(false));
    session.model_registry().auth_storage.register_oauth_provider("rpc-prompt-fixture", Arc::new(PromptOAuth { started: started.clone() }));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let handler = RpcConnectionHandler::create(session, Arc::new(move |event| { let _ = event_tx.send(event); }), Vec::new()).await;
    let response = send(&handler, json!({"id":"start-success","type":"login_start","provider":"rpc-prompt-fixture"})).await;
    assert_eq!(response["success"], true);
    let mut saw_auth_url = false;
    let prompt = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let event = event_rx.recv().await.expect("RPC event stream");
            if event == json!({"type":"auth_login_url","provider":"rpc-prompt-fixture","url":"https://auth.fixture.test/start"}) { saw_auth_url = true; }
            if event["method"] == "input" { break event; }
        }
    }).await.expect("login URL and prompt events");
    assert!(saw_auth_url);
    assert!(started.load(Ordering::SeqCst));
    let prompt_id = prompt["id"].as_str().expect("prompt request id");
    assert!(handler.handle_input_line(&json!({"type":"extension_ui_response","id":prompt_id,"value":"fixture-code"}).to_string()).await.expect("answer prompt").is_none());
    let end = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let event = event_rx.recv().await.expect("RPC event stream");
            if event["type"] == "auth_login_end" { break event; }
        }
    }).await.expect("terminal auth event");
    assert_eq!(end, json!({"type":"auth_login_end","provider":"rpc-prompt-fixture","success":true}));
    handler.dispose().await;
}
