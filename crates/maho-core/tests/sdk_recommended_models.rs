use std::sync::{Arc, Mutex};
use maho_ext_api::Extension;

#[tokio::test]
async fn native_recommended_startup_awaits_model_switch_and_preserves_handler_order() {
    let dir = tempfile::tempdir().expect("isolated SDK");
    let cwd = dir.path().to_string_lossy().into_owned();
    let agent_dir = dir.path().join("agent");
    std::fs::create_dir(&agent_dir).expect("agent directory");
    std::fs::write(agent_dir.join("settings.json"), r#"{"recommendedModels":["recommended-fixture"]}"#).expect("settings");
    let models = ["initial-fixture", "recommended-fixture"].map(|id| serde_json::from_value::<maho_ai::types::Model>(serde_json::json!({
        "id":id,"name":id,"api":"faux","provider":"faux","baseUrl":"",
        "reasoning":false,"input":["text"],"contextWindow":128000,"maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
    })).expect("model"));
    let provider = maho_ai::providers::faux::faux_provider(maho_ai::providers::faux::RegisterFauxProviderOptions {
        models: Some(models.iter().map(|model| maho_ai::providers::faux::FauxModelDefinition {
            id: model.id.clone(), context_window: Some(128000), max_tokens: Some(4096),
            ..Default::default()
        }).collect()), tokens_per_second: Some(0.0), ..Default::default()
    });
    let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions::default());
    runtime.register_native_provider(provider.provider.clone());
    runtime.register_provider("faux", maho_core::provider_composer::ProviderConfigInput {
        config: maho_core::model_config_schema::ModelsJsonProvider { api_key: Some("faux-test".into()), ..Default::default() },
        ..Default::default()
    }).expect("configured provider");
    let order = Arc::new(Mutex::new(Vec::new()));
    let captured = order.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "recommended-models".into(), source_info: Default::default(),
        factory: Arc::new(move |api| {
            maho_ext_recommended_models::RecommendedModels.register(api);
            let selected = captured.clone();
            api.on(maho_ext_api::EventKind::ModelSelect, Arc::new(move |_, _| {
                selected.lock().expect("order").push("model-select");
                Box::pin(async { Ok(maho_ext_api::EventResult::None) })
            }));
            let started = captured.clone();
            api.on(maho_ext_api::EventKind::SessionStart, Arc::new(move |event, ctx| {
                let maho_ext_api::ExtensionEvent::SessionStart(event) = event else { panic!("startup event"); };
                assert_eq!(event.initial_model_provenance.as_deref(), Some("first-available"));
                assert_eq!(event.previous_session_file.as_deref(), Some("fixture-origin.jsonl"));
                started.lock().expect("order").push("startup-after-switch");
                let id = ctx.model.as_ref().expect("live model").id.clone();
                Box::pin(async move {
                    assert_eq!(id, "recommended-fixture");
                    Ok(maho_ext_api::EventResult::None)
                })
            }));
            Box::pin(async { Ok(()) })
        }),
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), maho_core::sdk::create_agent_session(
        maho_core::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd.clone()), agent_dir: Some(agent_dir.to_string_lossy().into_owned()),
            model: Some(models[0].clone()), model_runtime: Some(runtime),
            initial_model_provenance: Some("first-available".into()),
            session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
            session_start_event: Some(maho_ext_api::SessionStartEvent {
                reason: maho_ext_api::SessionReason::Startup,
                initial_model_provenance: Some("explicit".into()), previous_session_file: Some("fixture-origin.jsonl".into()),
            }), extension_factories: vec![factory], tools: Some(Vec::new()), ..Default::default()
        })).await;
    let selected = if let Ok(Ok(created)) = &result {
        let model = created.session.model();
        created.session.dispose().await;
        Some(model)
    } else { None };
    assert!(result.is_ok(), "native startup must finish its awaited model switch");
    assert!(result.expect("bounded startup").is_ok(), "SDK startup");
    assert_eq!(selected.expect("selected model").id, "recommended-fixture");
    assert_eq!(*order.lock().expect("order"), ["model-select", "startup-after-switch"]);
    assert!(provider.get_call_log().is_empty());
}
