use maho_core::{model_runtime::{CreateModelRuntimeOptions, ModelRuntime}, sdk::{CreateAgentSessionOptions, NoToolsMode, create_agent_session}, session_manager::SessionManager, settings_manager::{InMemorySettingsStorage, SettingsManager}};
use maho_rpc::connection_handler::handle_input_line;
use serde_json::{Value, json};

async fn command(session: &maho_core::agent_session::AgentSession, value: Value) -> Value {
    serde_json::from_str(&handle_input_line(session, &value.to_string()).await.expect("dispatch").expect("response")).expect("JSON response")
}

#[tokio::test]
async fn persisted_fork_clone_tree_and_import_preserve_source() {
    let temp = tempfile::tempdir().expect("root");
    let cwd = temp.path().to_string_lossy().into_owned();
    let directory = temp.path().join("sessions");
    let mut manager = SessionManager::create(&cwd, Some(directory.to_str().expect("directory")), None);
    manager.append_thinking_level_change("off", None);
    let user = manager.append_message(json!({"role":"user","content":[{"type":"text","text":"source user"}],"timestamp":0}));
    let assistant = manager.append_message(json!({"role":"assistant","content":[{"type":"text","text":"source answer"}],"api":"openai-completions","provider":"faux","model":"faux-1","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":0}));
    let source = manager.session_file().expect("source path").to_owned();
    let bytes = std::fs::read(&source).expect("persisted source");
    let provider = maho_ai::providers::faux::faux_provider(Default::default());
    let model = provider.get_model(Some("faux-1")).expect("model");
    let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions { providers: Some(vec![provider.provider]), ..Default::default() });
    let session = create_agent_session(CreateAgentSessionOptions { cwd: Some(cwd.clone()), agent_dir: Some(cwd), model_runtime: Some(runtime), model: Some(model), session_manager: Some(manager), settings_manager: Some(SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), false)), no_tools: Some(NoToolsMode::All), auto_title_sessions: Some(false), ..Default::default() }).await.expect("session").session;
    let response = command(&session, json!({"type":"get_tree"})).await;
    assert_eq!(response["success"], true);
    assert_eq!(response["data"]["leafId"], assistant["id"]);
    assert_eq!(response["data"]["tree"].as_array().expect("root forest").len(), 1);
    assert!(response["data"]["tree"][0].get("label").is_none());
    assert!(response["data"]["tree"].to_string().contains("source user"));
    let response = command(&session, json!({"type":"fork","entryId":"absent"})).await;
    assert_eq!(response["success"], false);
    assert_eq!(session.session_file().as_deref(), Some(source.as_str()));
    let response = command(&session, json!({"type":"clone"})).await;
    assert_eq!(response["data"], json!({"cancelled":false}));
    let cloned = session.session_file().expect("clone path");
    assert_ne!(cloned, source);
    assert!(std::path::Path::new(&cloned).exists());
    assert_eq!(session.messages().len(), 2);
    assert_eq!(std::fs::read(&source).expect("source unchanged"), bytes);
    let response = command(&session, json!({"type":"fork","entryId":user["id"],"position":"at"})).await;
    assert_eq!(response["success"], true);
    assert_eq!(session.messages().len(), 1);
    let response = command(&session, json!({"type":"import_jsonl","inputPath":source})).await;
    assert_eq!(response["data"], json!({"cancelled":false}));
    assert_eq!(session.messages().len(), 2);
    let response = command(&session, json!({"type":"navigate_tree","targetId":"absent"})).await;
    assert_eq!(response["success"], false);
    assert_eq!(response["errorCode"], "not_found");
    assert_eq!(std::fs::read(&source).expect("source unchanged"), bytes);
    let response = command(&session, json!({"type":"new_session","parentSession":source})).await;
    assert_eq!(response["data"], json!({"cancelled":false}));
    assert!(session.messages().is_empty());
    session.dispose().await;
}
