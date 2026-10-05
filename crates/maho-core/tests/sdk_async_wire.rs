use std::sync::{Arc, Mutex};
use futures::FutureExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn native_sdk_abort_drops_pending_payload_hook_without_transmitting() {
    struct HookLifetime(Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>);
    impl Drop for HookLifetime {
        fn drop(&mut self) {
            if let Some(sender) = self.0.lock().expect("drop observer").take() { let _ = sender.send(()); }
        }
    }
    maho_ai::api_registry::register_builtin_api_provider("openai-completions",
        maho_ai::api::openai_completions_lazy::open_ai_completions_api());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let dir = tempfile::tempdir().expect("isolated SDK");
    let cwd = dir.path().to_string_lossy().into_owned();
    let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        providers: Some(Vec::new()), ..Default::default()
    });
    runtime.register_provider("wire-cancel-fixture", maho_core::provider_composer::ProviderConfigInput {
        config: maho_core::model_config_schema::ModelsJsonProvider {
            api: Some("openai-completions".into()), base_url: Some(format!("http://{address}/v1")), api_key: Some("fixture-key".into()),
            models: Some(vec![maho_core::model_config_schema::ModelsJsonModel { id: "fixture".into(), ..Default::default() }]),
            ..Default::default()
        }, ..Default::default()
    }).expect("provider");
    let model = runtime.get_model("wire-cancel-fixture", "fixture").expect("model");
    let (entered, mut entries) = tokio::sync::mpsc::unbounded_channel();
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    let dropped = Arc::new(Mutex::new(Some(dropped_tx)));
    let hook_dropped = dropped.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "wire-cancel-hooks".into(), source_info: Default::default(),
        factory: Arc::new(move |api| {
            let entered = entered.clone();
            let dropped = hook_dropped.clone();
            api.on(maho_ext_api::EventKind::BeforeProviderRequest, Arc::new(move |_, _| {
                let entered = entered.clone();
                let lifetime = HookLifetime(dropped.clone());
                Box::pin(async move {
                    let _lifetime = lifetime;
                    entered.send(()).expect("observer");
                    std::future::pending().await
                })
            }));
            Box::pin(async { Ok(()) })
        }),
    };
    let session = maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        model: Some(model), model_runtime: Some(runtime), tools: Some(Vec::new()), extension_factories: vec![factory],
        session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
        ..Default::default()
    }).await.expect("native SDK").session;
    let result = std::panic::AssertUnwindSafe(tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(session.prompt("cancel before transmission", Default::default()), async {
            entries.recv().await.expect("payload hook entered");
            session.abort().await;
        })
    })).catch_unwind().await;
    let dropped = tokio::time::timeout(std::time::Duration::from_secs(3), dropped_rx).await;
    session.dispose().await;
    let result = match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    let (prompt_result, ()) = result.expect("bounded aborted prompt");
    if let Err(error) = prompt_result {
        assert!(error.to_lowercase().contains("abort") || error.to_lowercase().contains("cancel"), "{error}");
    }
    dropped.expect("bounded callback teardown").expect("callback dropped after abort");
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(listener.poll_accept(cx).is_pending())).await,
        "cancelled hook must not transmit");
}

#[tokio::test]
async fn native_sdk_awaits_generated_payload_and_response_hooks_on_real_wire() {
    maho_ai::api_registry::register_builtin_api_provider("openai-completions",
        maho_ai::api::openai_completions_lazy::open_ai_completions_api());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let dir = tempfile::tempdir().expect("isolated SDK");
    let cwd = dir.path().to_string_lossy().into_owned();
    let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        providers: Some(Vec::new()), ..Default::default()
    });
    runtime.register_provider("wire-fixture", maho_core::provider_composer::ProviderConfigInput {
        config: maho_core::model_config_schema::ModelsJsonProvider {
            api: Some("openai-completions".into()), base_url: Some(format!("http://{address}/v1")), api_key: Some("fixture-key".into()),
            models: Some(vec![maho_core::model_config_schema::ModelsJsonModel { id: "fixture".into(), ..Default::default() }]),
            ..Default::default()
        }, ..Default::default()
    }).expect("provider");
    let model = runtime.get_model("wire-fixture", "fixture").expect("model");
    let (entered, mut entries) = tokio::sync::mpsc::unbounded_channel();
    let payload_release = Arc::new(tokio::sync::Semaphore::new(0));
    let response_release = Arc::new(tokio::sync::Semaphore::new(0));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let payload_gate = payload_release.clone();
    let response_gate = response_release.clone();
    let metadata = observed.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "wire-hooks".into(), source_info: Default::default(),
        factory: Arc::new(move |api| {
            let entered_payload = entered.clone();
            let gate = payload_gate.clone();
            let metadata = metadata.clone();
            api.on(maho_ext_api::EventKind::BeforeProviderRequest, Arc::new(move |event, _| {
                let gate = gate.clone();
                let entered = entered_payload.clone();
                let metadata = metadata.clone();
                Box::pin(async move {
                    let maho_ext_api::ExtensionEvent::BeforeProviderRequest { payload, model, .. } = event else { panic!("payload event"); };
                    metadata.lock().expect("metadata").push(model.as_ref().map(|model| model.id.clone()));
                    entered.send("payload").expect("observer");
                    gate.acquire().await.expect("release").forget();
                    let mut next = payload.clone(); next["user"] = "sdk-async-wire".into();
                    Ok(maho_ext_api::EventResult::ProviderPayload(next))
                })
            }));
            let gate = response_gate.clone();
            let entered = entered.clone();
            api.on(maho_ext_api::EventKind::AfterProviderResponse, Arc::new(move |event, _| {
                let gate = gate.clone(); let entered = entered.clone();
                Box::pin(async move {
                    let maho_ext_api::ExtensionEvent::AfterProviderResponse { status, .. } = event else { panic!("response event"); };
                    assert_eq!(*status, 200);
                    entered.send("response").expect("observer");
                    gate.acquire().await.expect("release").forget();
                    Ok(maho_ext_api::EventResult::None)
                })
            }));
            Box::pin(async { Ok(()) })
        }),
    };
    let session = maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        model: Some(model), model_runtime: Some(runtime), tools: Some(Vec::new()), extension_factories: vec![factory],
        session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
        ..Default::default()
    }).await.expect("native SDK").session;
    let result = std::panic::AssertUnwindSafe(tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let prompt = session.prompt("wire request", Default::default());
        let wire = async {
            assert_eq!(entries.recv().await, Some("payload"));
            assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(listener.poll_accept(cx).is_pending())).await,
                "no transmission before hook settles");
            payload_release.add_permits(1);
            let (mut connection, _) = listener.accept().await.expect("request");
            let mut bytes = Vec::new(); let mut chunk = [0; 4096];
            let payload = loop {
                let count = connection.read(&mut chunk).await.expect("read");
                assert_ne!(count, 0); bytes.extend_from_slice(&chunk[..count]);
                if let Some(end) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length = headers.lines().find_map(|line| line.split_once(':').filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse::<usize>().expect("length"))).expect("length header");
                    if bytes.len() >= end + 4 + length { break serde_json::from_slice::<serde_json::Value>(&bytes[end + 4..end + 4 + length]).expect("wire JSON"); }
                }
            };
            assert_eq!(payload["user"], "sdk-async-wire");
            let body = "data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"wire complete\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            connection.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.expect("response");
            assert_eq!(entries.recv().await, Some("response"));
            assert!(!session.messages().iter().any(|message| matches!(message, maho_agent::types::AgentMessage::Llm(maho_ai::types::Message::Assistant(message)) if maho_ai::utils::text::content_text(&message.content, "") == "wire complete")));
            response_release.add_permits(1);
        };
        tokio::join!(prompt, wire).0
    })).catch_unwind().await;
    session.dispose().await;
    drop(listener);
    let result = match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    result.expect("bounded wire lifecycle").expect("prompt");
    assert_eq!(observed.lock().expect("metadata").as_slice(), [Some("fixture".into())]);
}

#[tokio::test]
async fn native_sdk_payload_failure_is_reported_without_transmission() {
    maho_ai::api_registry::register_builtin_api_provider("openai-completions",
        maho_ai::api::openai_completions_lazy::open_ai_completions_api());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let dir = tempfile::tempdir().expect("isolated SDK");
    let cwd = dir.path().to_string_lossy().into_owned();
    let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        providers: Some(Vec::new()), ..Default::default()
    });
    runtime.register_provider("wire-error-fixture", maho_core::provider_composer::ProviderConfigInput {
        config: maho_core::model_config_schema::ModelsJsonProvider {
            api: Some("openai-completions".into()), base_url: Some(format!("http://{address}/v1")), api_key: Some("fixture-key".into()),
            models: Some(vec![maho_core::model_config_schema::ModelsJsonModel { id: "fixture".into(), ..Default::default() }]),
            ..Default::default()
        }, ..Default::default()
    }).expect("provider");
    let model = runtime.get_model("wire-error-fixture", "fixture").expect("model");
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let captured = calls.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "wire-error-hooks".into(), source_info: Default::default(),
        factory: Arc::new(move |api| {
            let calls = captured.clone();
            api.on(maho_ext_api::EventKind::BeforeProviderRequest, Arc::new(move |event, _| {
                let calls = calls.clone();
                *event = maho_ext_api::ExtensionEvent::Context { messages: Vec::new() };
                Box::pin(async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(maho_ext_api::EventResult::None)
                })
            }));
            Box::pin(async { Ok(()) })
        }),
    };
    let session = maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        model: Some(model), model_runtime: Some(runtime), tools: Some(Vec::new()), extension_factories: vec![factory],
        session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
        ..Default::default()
    }).await.expect("native SDK").session;
    let result = tokio::time::timeout(std::time::Duration::from_secs(10),
        session.prompt("reject generated payload", Default::default())).await;
    let messages = session.messages();
    session.dispose().await;
    let prompt = result.expect("bounded failed prompt");
    let reported = prompt.err().is_some_and(|error| error.contains("provider handler replaced event kind"))
        || messages.iter().any(|message| matches!(message,
            maho_agent::types::AgentMessage::Llm(maho_ai::types::Message::Assistant(message))
            if message.error_message.as_deref().is_some_and(|error| error.contains("provider handler replaced event kind"))));
    assert!(reported, "hook error must reach the SDK caller or assistant error record");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(listener.poll_accept(cx).is_pending())).await,
        "failed hook must not transmit");
}
