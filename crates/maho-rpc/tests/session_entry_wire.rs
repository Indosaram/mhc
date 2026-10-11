use maho_core::{model_runtime::{CreateModelRuntimeOptions, ModelRuntime}, sdk::{CreateAgentSessionOptions, NoToolsMode, create_agent_session}, session_manager::SessionManager, settings_manager::{InMemorySettingsStorage, SettingsManager}};
use maho_rpc::connection_handler::handle_input_line;
use serde_json::{Value, json};

#[tokio::test]
async fn fast_mode_rpc_uses_registered_action_and_reports_refusal() {
    use std::sync::{Arc, Mutex};
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().to_string_lossy().into_owned();
    let provider = maho_ai::providers::faux::faux_provider(Default::default());
    let model = provider.get_model(Some("faux-1")).unwrap();
    let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
        models_path: Some(temp.path().join("models.json")), auth_path: Some(temp.path().join("auth.json")),
        providers: Some(vec![provider.provider]), ..Default::default()
    });
    let calls = Arc::new(Mutex::new(Vec::new()));
    let captured = calls.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "<builtin:service-tier>".into(), source_info: Default::default(),
        factory: Arc::new(move |api| {
            let calls = captured.clone();
            let sender = Arc::new(maho_ext_api::ExtensionApi::new(api.registered.clone(), api.profile.clone(), api.events.clone(), api.runtime.clone()));
            Box::pin(async move {
                api.register_fast_mode_action(Arc::new(move |enabled, context| {
                    let calls = calls.clone();
                    let sender = sender.clone();
                    Box::pin(async move {
                        calls.lock().unwrap().push(enabled);
                        if enabled { sender.set_session_fast_mode(true)?; }
                        Ok(maho_ext_api::FastModeResult {
                            enabled: true, applied: enabled, recorded_tier: maho_ext_api::ServiceTier::Priority,
                            message: "refused-by-action".into(), memory_model: context.model,
                        })
                    })
                }))
            })
        }),
    };
    let session = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(cwd.clone()), model_runtime: Some(runtime), model: Some(model.clone()),
        session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
        settings_manager: Some(SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), false)),
        extension_factories: vec![factory], no_tools: Some(NoToolsMode::All), auto_title_sessions: Some(false), ..Default::default()
    }).await.unwrap().session;
    let response: Value = serde_json::from_str(&handle_input_line(&session, "{\"type\":\"set_fast_mode\",\"enabled\":true}").await.unwrap().unwrap()).unwrap();
    assert_eq!(response["success"], true);
    assert_eq!(response["data"], json!({"enabled":true,"serviceTier":"priority","provider":model.provider,"modelId":model.id}));
    let response: Value = serde_json::from_str(&handle_input_line(&session, "{\"type\":\"set_fast_mode\",\"enabled\":false}").await.unwrap().unwrap()).unwrap();
    assert_eq!(response["success"], false);
    assert_eq!(response["error"], "refused-by-action");
    assert!(session.is_fast_mode_active(), "refusal must not directly toggle session fast mode");
    assert_eq!(*calls.lock().unwrap(), vec![true, false]);
    session.dispose().await;
}

#[tokio::test]
async fn entries_export_and_cleanup_use_the_real_session() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().to_string_lossy().into_owned();
    let provider = maho_ai::providers::faux::faux_provider(Default::default());
    let model = provider.get_model(Some("faux-1")).unwrap();
    let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
        models_path: Some(temp.path().join("models.json")), auth_path: Some(temp.path().join("auth.json")),
        providers: Some(vec![provider.provider]), ..Default::default()
    });
    let session = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(cwd.clone()), model_runtime: Some(runtime), model: Some(model.clone()),
        session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
        settings_manager: Some(SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), false)),
        no_tools: Some(NoToolsMode::All), auto_title_sessions: Some(false), ..Default::default()
    }).await.unwrap().session;
    for kind in ["set_favorite_models", "set_scoped_models"] {
        let command=json!({"type":kind,"models":[{"model":model,"serviceTier":"priority"}]});
        let response:Value=serde_json::from_str(&handle_input_line(&session,&command.to_string()).await.unwrap().unwrap()).unwrap();
        assert_eq!(response["success"],true);
    }
    assert_eq!(session.favorite_models().len(),1);
    assert_eq!(session.scoped_models().len(),1);
    let entry = json!({"id":"entry","parentId":null,"type":"message","timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":[{"type":"text","text":"wire"}],"timestamp":0}});
    let response: Value = serde_json::from_str(&handle_input_line(&session, &json!({"type":"append_session_entry","entry":entry}).to_string()).await.unwrap().unwrap()).unwrap();
    assert_eq!(response["success"], true);
    assert_eq!(session.messages().len(), 1);
    let path = temp.path().join("export.jsonl");
    let response: Value = serde_json::from_str(&handle_input_line(&session, &json!({"type":"export_jsonl","outputPath":path}).to_string()).await.unwrap().unwrap()).unwrap();
    assert_eq!(response["data"]["path"], path.to_string_lossy().as_ref());
    let exported = std::fs::read_to_string(&path).unwrap();
    assert!(exported.lines().map(|line|serde_json::from_str::<Value>(line).unwrap()).any(|record|record["message"]["content"][0]["text"] == "wire"));
    let response: Value = serde_json::from_str(&handle_input_line(&session, &json!({"type":"cleanup_bash_output","path":path}).to_string()).await.unwrap().unwrap()).unwrap();
    assert_eq!(response["success"], true);
    assert!(!path.exists());
    let response: Value = serde_json::from_str(&handle_input_line(&session, "{\"type\":\"get_fast_mode\"}").await.unwrap().unwrap()).unwrap();
    assert_eq!(response["data"], json!({"enabled":false,"serviceTier":null}));
    let response:Value=serde_json::from_str(&handle_input_line(&session,"{\"type\":\"reload\",\"id\":\"reload-request\"}").await.unwrap().unwrap()).unwrap();
    assert_eq!(response["id"],"reload-request");
    assert_eq!(response["command"],"reload");
    assert_eq!(response["success"],true);
    assert_eq!(response["data"],json!({"cancelled":false}));
    session.dispose().await;
}
