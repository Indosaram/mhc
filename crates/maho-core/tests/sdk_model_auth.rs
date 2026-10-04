use std::sync::{Arc, Mutex};

#[tokio::test]
async fn native_sdk_registry_resolves_distinct_models_and_reports_failed_auth() {
    let dir = tempfile::tempdir().expect("isolated SDK");
    let cwd = dir.path().to_string_lossy().into_owned();
    let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(
        maho_core::model_runtime::CreateModelRuntimeOptions { providers: Some(Vec::new()), ..Default::default() });
    let definitions = ["first", "second"].map(|id| maho_core::model_config_schema::ModelsJsonModel {
        id: id.into(), upstream_model_id: Some(format!("wire-{id}")),
        headers: Some(std::collections::BTreeMap::from([("x-model".into(), id.into())])),
        extra_body: Some(serde_json::Map::from_iter([("route".into(), id.into())])),
        service_tier: Some(maho_ai::types::ServiceTierPreference::Priority), ..Default::default()
    }).to_vec();
    runtime.register_provider("resolved-fixture", maho_core::provider_composer::ProviderConfigInput {
        config: maho_core::model_config_schema::ModelsJsonProvider {
            api: Some("openai-completions".into()), api_key: Some("fixture-key".into()),
            base_url: Some("http://127.0.0.1:1/v1".into()),
            headers: Some(std::collections::BTreeMap::from([("x-provider".into(), "shared".into())])),
            models: Some(definitions), auth_header: Some(true),
            ..Default::default()
        }, ..Default::default()
    }).expect("resolved provider");
    runtime.register_provider("failed-fixture", maho_core::provider_composer::ProviderConfigInput {
        config: maho_core::model_config_schema::ModelsJsonProvider {
            api: Some("openai-completions".into()), base_url: Some("http://127.0.0.1:1".into()),
            auth_header: Some(true), models: Some(vec![maho_core::model_config_schema::ModelsJsonModel {
                id: "missing".into(), ..Default::default()
            }]),
            ..Default::default()
        }, ..Default::default()
    }).expect("missing auth provider");
    let models = vec![runtime.get_model("resolved-fixture", "first").expect("first model"),
        runtime.get_model("resolved-fixture", "second").expect("second model"),
        runtime.get_model("failed-fixture", "missing").expect("failed model")];
    let captured = Arc::new(Mutex::new(Vec::new()));
    let observed = captured.clone();
    let requests = models.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "scoped-auth-fixture".into(), source_info: Default::default(),
        factory: Arc::new(move |api| {
            let observed = observed.clone();
            let requests = requests.clone();
            api.on(maho_ext_api::EventKind::SessionStart, Arc::new(move |_, context| {
                let observed = observed.clone();
                let requests = requests.clone();
                let registry = context.model_registry.clone();
                Box::pin(async move {
                    for model in requests {
                        let result = registry.get_api_key_and_headers(&model).await;
                        observed.lock().expect("projection").push(result);
                    }
                    Ok(maho_ext_api::EventResult::None)
                })
            }));
            Box::pin(async { Ok(()) })
        }),
    };
    let created = tokio::time::timeout(std::time::Duration::from_secs(5), maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        model: Some(models[0].clone()), model_runtime: Some(runtime), tools: Some(Vec::new()),
        extension_factories: vec![factory],
        session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
        ..Default::default()
    })).await.expect("bounded model-auth startup").expect("native SDK startup");
    created.session.dispose().await;
    let results = std::mem::take(&mut *captured.lock().expect("projection"));
    assert_eq!(results.len(), 3);
    for (index, id) in ["first", "second"].into_iter().enumerate() {
        let result = results[index].as_ref().expect("resolved model auth");
        assert_eq!(result.auth.api_key.as_deref(), Some("fixture-key"));
        assert!(result.auth.base_url.is_none(), "configured catalog URL is not an auth URL override");
        assert_eq!(models[index].base_url, "http://127.0.0.1:1/v1");
        let headers = result.auth.headers.as_ref().expect("headers");
        assert_eq!(headers.get("x-model"), Some(&Some(id.into())));
        assert_eq!(headers.get("x-provider"), Some(&Some("shared".into())));
        assert_eq!(result.extra_body.as_ref().expect("body").get("route"), Some(&id.into()));
        assert_eq!(result.upstream_model_id.as_deref(), Some(format!("wire-{id}").as_str()));
        assert_eq!(result.service_tier, Some(maho_ai::types::ServiceTierPreference::Priority));
    }
    assert!(results[2].is_err(), "missing required auth must remain Failed");
}

#[tokio::test]
async fn native_sdk_registry_projects_model_headers_without_unresolved_auth_routing() {
    let dir = tempfile::tempdir().expect("isolated SDK");
    let cwd = dir.path().to_string_lossy().into_owned();
    let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(
        maho_core::model_runtime::CreateModelRuntimeOptions { providers: Some(Vec::new()), ..Default::default() });
    runtime.register_provider("header-only-fixture", maho_core::provider_composer::ProviderConfigInput {
        config: maho_core::model_config_schema::ModelsJsonProvider {
            api: Some("openai-completions".into()), base_url: Some("http://127.0.0.1:1".into()),
            ..Default::default()
        }, ..Default::default()
    }).expect("provider");
    let model: maho_ai::types::Model = serde_json::from_value(serde_json::json!({
        "id":"header-only","name":"header-only","api":"openai-completions",
        "provider":"header-only-fixture","baseUrl":"http://127.0.0.1:1",
        "reasoning":false,"input":["text"],"contextWindow":128000,"maxTokens":4096,
        "headers":{"x-model":"native"},"upstreamModelId":"unresolved-routing","serviceTier":"priority",
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
    })).expect("model");
    let captured = Arc::new(Mutex::new(None));
    let observed = captured.clone();
    let request_model = model.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "model-auth-fixture".into(), source_info: Default::default(),
        factory: Arc::new(move |api| {
            let observed = observed.clone();
            let model = request_model.clone();
            api.on(maho_ext_api::EventKind::SessionStart, Arc::new(move |_, ctx| {
                let observed = observed.clone();
                let registry = ctx.model_registry.clone();
                let model = model.clone();
                Box::pin(async move {
                    *observed.lock().expect("projection") = Some(registry.get_api_key_and_headers(&model).await);
                    Ok(maho_ext_api::EventResult::None)
                })
            }));
            Box::pin(async { Ok(()) })
        }),
    };
    let created = tokio::time::timeout(std::time::Duration::from_secs(3), maho_core::sdk::create_agent_session(
        maho_core::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            model: Some(model), model_runtime: Some(runtime),
            session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
            session_start_event: Some(maho_ext_api::SessionStartEvent {
                reason: maho_ext_api::SessionReason::Startup, initial_model_provenance: None, previous_session_file: None,
            }), extension_factories: vec![factory], tools: Some(Vec::new()), ..Default::default()
        })).await.expect("bounded startup").expect("SDK startup");
    created.session.dispose().await;
    let result = captured.lock().expect("projection").take().expect("registered startup projection").expect("resolved headers");
    assert!(result.auth.api_key.is_none());
    assert_eq!(result.auth.headers.expect("model headers").get("x-model"), Some(&Some("native".into())));
    assert!(result.upstream_model_id.is_none());
    assert!(result.service_tier.is_none());
    assert!(result.env.is_none());
}
