use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use maho_ext_api::{EventKind, EventResult, ExtensionEvent};

#[tokio::test(flavor = "current_thread")]
async fn pending_message_end_handler_does_not_block_renderer_snapshot() {
    let directory = tempfile::tempdir().expect("isolated SDK");
    let cwd = directory.path().to_string_lossy().into_owned();
    let provider = maho_ai::providers::faux::faux_provider(maho_ai::providers::faux::RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0), ..Default::default()
    });
    provider.set_responses(vec![maho_ai::providers::faux::faux_assistant_message("done", Default::default()).into()]);
    let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        providers: Some(Vec::new()), ..Default::default()
    });
    runtime.register_native_provider(provider.provider.clone());
    runtime.register_provider("faux", maho_core::provider_composer::ProviderConfigInput {
        config: maho_core::model_config_schema::ModelsJsonProvider { api_key: Some("fixture-auth".into()), ..Default::default() },
        ..Default::default()
    }).expect("faux auth");

    let entered = Arc::new(tokio::sync::Notify::new());
    let started = entered.notified();
    let release = Arc::new(tokio::sync::Notify::new());
    // Only the first message_end (the user prompt) is held; later message_end events of the same
    // turn must run through, or the prompt could never settle after the release.
    let hold_first = Arc::new(AtomicBool::new(true));
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "pending-message-end".into(), source_info: Default::default(),
        factory: Arc::new({
            let entered = entered.clone();
            let release = release.clone();
            let hold_first = hold_first.clone();
            move |api| {
                let entered = entered.clone();
                let release = release.clone();
                let hold_first = hold_first.clone();
                api.on(EventKind::MessageEnd, Arc::new(move |event, _| {
                    let entered = entered.clone();
                    let release = release.clone();
                    let hold = hold_first.swap(false, Ordering::SeqCst);
                    Box::pin(async move {
                        if hold {
                            entered.notify_one();
                            release.notified().await;
                        }
                        let ExtensionEvent::MessageEnd { .. } = event else { unreachable!() };
                        Ok(EventResult::None)
                    })
                }));
                Box::pin(async { Ok(()) })
            }
        }),
    };
    let session = maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(directory.path().join("agent").to_string_lossy().into_owned()),
        model: Some(provider.get_model(Some("faux-1")).expect("model")), model_runtime: Some(runtime),
        session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
        tools: Some(Vec::new()), extension_factories: vec![factory], ..Default::default()
    }).await.expect("SDK session").session;

    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let prompt_session = session.clone();
        let prompt = tokio::task::spawn_local(async move {
            prompt_session.prompt("complete turn", Default::default()).await
        });
        tokio::time::timeout(Duration::from_secs(5), started).await.expect("message_end handler started");
        let snapshot = tokio::time::timeout(Duration::from_secs(2), session.native_tool_renderers_snapshot::<(), serde_json::Value>()).await;
        release.notify_one();
        let prompt_result = tokio::time::timeout(Duration::from_secs(5), prompt).await;
        assert!(snapshot.is_ok(), "renderer snapshot blocked while handler was pending");
        assert!(prompt_result.is_ok(), "prompt did not complete after releasing handler");
        assert!(prompt_result.expect("checked prompt timeout").expect("prompt task").is_ok());
    }).await;
    session.dispose().await;
}
