use std::sync::{Arc, Mutex};

#[tokio::test]
async fn configured_completion_callback_consumes_native_sdk_context() {
    let dir = tempfile::tempdir().expect("isolated SDK");
    let cwd = dir.path().to_string_lossy().into_owned();
    let provider = maho_ai::providers::faux::faux_provider(maho_ai::providers::faux::RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0), models: Some(["first", "second"].map(|id|
            maho_ai::providers::faux::FauxModelDefinition { id: id.into(), cost: Some(maho_ai::types::ModelCost {
                input: 1.0, output: 2.0, ..Default::default()
            }), ..Default::default() }).to_vec()), ..Default::default()
    });
    provider.set_responses((0..3).map(|_| maho_ai::providers::faux::faux_assistant_message("native completion", Default::default()).into()).collect());
    let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        providers: Some(Vec::new()), ..Default::default()
    });
    runtime.register_native_provider(provider.provider.clone());
    runtime.register_provider("faux", maho_core::provider_composer::ProviderConfigInput {
        config: maho_core::model_config_schema::ModelsJsonProvider { api_key: Some("fixture-key".into()), ..Default::default() },
        ..Default::default()
    }).expect("configured provider");
    let captured = Arc::new(Mutex::new(None));
    let observed = captured.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "completion-context".into(), source_info: Default::default(),
        factory: Arc::new(move |api| {
            let observed = observed.clone();
            api.on(maho_ext_api::EventKind::SessionStart, Arc::new(move |_, context| {
                *observed.lock().expect("context") = Some(context.clone());
                Box::pin(async { Ok(maho_ext_api::EventResult::None) })
            }));
            Box::pin(async { Ok(()) })
        }),
    };
    let model = provider.get_model(Some("second")).expect("model");
    let session = maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        model: Some(model.clone()), model_runtime: Some(runtime), extension_factories: vec![factory],
        tools: Some(Vec::new()), session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
        ..Default::default()
    }).await.expect("native SDK").session;
    let context = captured.lock().expect("context").take().expect("registered context");
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut results = Vec::new();
        for tier in [None, Some("smol"), Some("slow")] {
            results.push(maho_cli::cli::codemode_services::complete(
                maho_codemode::completion::handler::CompletionRequest { prompt: "fixture".into(), model: tier.map(str::to_owned),
                    system: None, schema: None, opts: None }, context.clone()).await?);
        }
        let mut aborted_context = context.clone();
        let signal = maho_ext_api::AbortSignal::default();
        signal.abort();
        aborted_context.signal = Some(signal);
        assert!(maho_cli::cli::codemode_services::complete(
            maho_codemode::completion::handler::CompletionRequest { prompt: "aborted fixture".into(), model: None,
                system: None, schema: None, opts: None }, aborted_context).await.is_err());
        Ok::<_, maho_codemode::completion::handler::CompletionError>(results)
    }).await;
    session.dispose().await;
    let result = result.expect("bounded completion").expect("configured completion");
    assert_eq!(result[0]["text"], "native completion");
    assert_eq!(result[0]["details"]["model"], format!("{}/{}", model.provider, model.id));
    for response in &result[1..] {
        assert_eq!(response["text"], "native completion");
        assert_eq!(response["details"]["model"], "faux/first", "cost ties retain registry order");
    }
}
