use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::load_script, faux_session::FauxSession};

#[tokio::test]
async fn faux_registered_component_preserves_idle_prompt_and_provider_reply() {
    let script = load_script("hello").expect("shared faux script");
    let reference: serde_json::Value = serde_json::from_str(include_str!("reference-hello.json"))
        .expect("generated pinned reference");
    let session = FauxSession::new(script).with_extension("omo").with_native_extension(
        NativeExtensionFactory {
            path: "<fallback-architect>".into(),
            source_info: Default::default(),
            extension: Box::new(maho_omo_fallback_architect::FallbackArchitectComponent::default()),
        },
    );
    let actual = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await.expect("bounded native session").expect("registered component run");
    let messages = actual["messages"].as_array().expect("native messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    let expected: Vec<_> = reference["entries"].as_array().expect("reference entries").iter()
        .filter_map(|entry| entry.get("message"))
        .filter(|message| matches!(message["role"].as_str(), Some("user" | "assistant")))
        .collect();
    assert_eq!(messages[0]["content"], expected[0]["content"]);
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"], expected[1]["content"]);
    assert!(actual["events"].as_array().expect("session events").iter()
        .any(|event| event["type"] == "agent_end"));
}

#[tokio::test]
async fn native_non_fable_refusal_falls_back_without_architect_injection() {
    use std::sync::{Arc, Mutex};
    use maho_ai::providers::faux::*;
    use maho_ext_api::*;
    struct Observed(Arc<Mutex<Vec<EventKind>>>);
    impl Extension for Observed {
        fn register(&self, api: &mut ExtensionApi) {
            maho_omo_fallback_architect::FallbackArchitectComponent::default().register(api);
            for kind in [EventKind::MessageEnd, EventKind::ModelSelect] {
                let events = self.0.clone();
                api.on(kind, Arc::new(move |_, _| {
                    events.lock().expect("events").push(kind);
                    Box::pin(async { Ok(EventResult::None) })
                }));
            }
        }
    }
    let root = tempfile::tempdir().expect("private cwd");
    let cwd = root.path().to_string_lossy().into_owned();
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        models: Some(["faux-1", "faux-2"].into_iter().map(|id| FauxModelDefinition { id: id.into(), ..Default::default() }).collect()),
        ..Default::default()
    }).expect("valid faux provider");
    let mut refusal = faux_assistant_message("", FauxAssistantMessageOptions { stop_reason: Some(maho_ai::types::StopReason::Error), timestamp: Some(0), ..Default::default() });
    refusal.stop_details = Some(serde_json::from_value(serde_json::json!({"type":"refusal"})).expect("typed refusal"));
    provider.set_responses(vec![FauxResponseStep::Message(Box::new(refusal)), FauxResponseStep::Message(Box::new(faux_assistant_message("recovered", FauxAssistantMessageOptions { timestamp: Some(0), ..Default::default() })))]);
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(root.path().join("models.json")), auth_path: Some(root.path().join("auth.json")),
        credentials: Some(Arc::new(maho_core::auth_storage::AuthStorage::in_memory([("faux".into(), serde_json::json!({"type":"api_key","key":"fixture"}))].into_iter().collect()))),
        providers: Some(vec![provider.provider.clone()]),
            ..Default::default()
        }).expect("valid model runtime");
    let mut settings = maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()), false);
    settings.apply_overrides(&serde_json::Map::from_iter([("retry".into(), serde_json::json!({"enabled":true,"maxRetries":0,"modelFallback":true,"baseDelayMs":0,"fallbackChains":{"faux/faux-1":["faux/faux-2"]}}))]));
    let events = Arc::new(Mutex::new(Vec::new()));
    let loaded = maho_ext_host::loader::load_extensions(vec![NativeExtensionFactory { path: "<architect>".into(), source_info: Default::default(), extension: Box::new(Observed(events.clone())) }], root.path(), Default::default());
    let session = maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(root.path().join("agent").to_string_lossy().into_owned()),
        model_runtime: Some(runtime), model: provider.get_model(Some("faux-1")),
        session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
        settings_manager: Some(settings), loaded_extensions: Some(loaded), tools: Some(Vec::new()), auto_title_sessions: Some(false), ..Default::default()
    }).await.expect("native session").session;
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.prompt("hi", Default::default())).await;
    let selected = session.model().id;
    let messages = session.messages();
    session.emit_session_shutdown(SessionReason::Quit).await;
    session.dispose().await;
    result.expect("bounded fallback").expect("prompt");
    assert_eq!(selected, "faux-2");
    let events = events.lock().expect("events");
    assert!(events.contains(&EventKind::MessageEnd));
    assert!(events.contains(&EventKind::ModelSelect));
    assert!(!messages.iter().any(|message| serde_json::to_value(message).expect("message").to_string().contains("omo-fallback-architect:")));
}


