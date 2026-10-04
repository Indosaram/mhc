use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};
use maho_ext_api::Extension;

#[tokio::test]
async fn native_permission_factory_enforces_provider_tool_admission() {
    for allowed in [false, true] {
    let directory = tempfile::tempdir().expect("isolated SDK");
    let cwd = directory.path().to_string_lossy().into_owned();
    let agent_dir = directory.path().join("agent");
    std::fs::create_dir(&agent_dir).expect("agent directory");
    std::fs::write(agent_dir.join("settings.json"), serde_json::json!({"permission":{"read":if allowed { "allow" } else { "ask" }}}).to_string())
        .expect("permission rules");
    let path = directory.path().join("fixture.txt").to_string_lossy().into_owned();
    let provider = maho_ai::providers::faux::faux_provider(maho_ai::providers::faux::RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0), ..Default::default()
    });
    let mut request = maho_ai::providers::faux::faux_assistant_message("", Default::default());
    request.stop_reason = maho_ai::types::StopReason::ToolUse;
    request.content = vec![maho_ai::providers::faux::faux_tool_call("read",
        serde_json::Map::from_iter([("path".into(), path.clone().into())]), Some("permission-fixture"))];
    provider.set_responses(vec![request.into(), maho_ai::providers::faux::faux_assistant_message("done", Default::default()).into()]);
    let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        providers: Some(Vec::new()), ..Default::default()
    });
    runtime.register_native_provider(provider.provider.clone());
    runtime.register_provider("faux", maho_core::provider_composer::ProviderConfigInput {
        config: maho_core::model_config_schema::ModelsJsonProvider { api_key: Some("fixture-auth".into()), ..Default::default() },
        ..Default::default()
    }).expect("faux auth");
    let calls = Arc::new(AtomicUsize::new(0));
    let executed = calls.clone();
    let admissions = Arc::new(Mutex::new(Vec::new()));
    let recorded = admissions.clone();
    let subscriptions = Arc::new(Mutex::new(Vec::new()));
    let retained_subscriptions = subscriptions.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "native-permission-fixture".into(), source_info: Default::default(),
        factory: Arc::new(move |api| {
            maho_ext_permission_system::PermissionSystem.register(api);
            let recorded = recorded.clone();
            retained_subscriptions.lock().expect("subscriptions").push(api.events.on("permission_asked",
                Arc::new(move |event| recorded.lock().expect("admission").push(event.clone()))));
            let executed = executed.clone();
            api.register_tool(maho_ext_api::ToolDefinition::new("read", "fixture read",
                serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
                Arc::new(move |_| {
                    executed.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async { Ok(maho_ext_api::ToolResult::text("authorized execution")) })
                })));
            Box::pin(async { Ok(()) })
        }),
    };
    let session = tokio::time::timeout(std::time::Duration::from_secs(5),
        maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(agent_dir.to_string_lossy().into_owned()),
        model: Some(provider.get_model(Some("faux-1")).expect("model")), model_runtime: Some(runtime),
        session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
        tools: Some(vec!["read".into()]), extension_factories: vec![factory], ..Default::default()
    })).await.expect("bounded permission factory startup").expect("SDK session").session;
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), session.prompt("read fixture", Default::default())).await;
    let messages = session.messages();
    session.dispose().await;
    result.expect("bounded turn").expect("provider turn");
    assert_eq!(calls.load(Ordering::SeqCst), usize::from(allowed));
    assert_eq!(admissions.lock().expect("admission").len(), usize::from(!allowed));
    if !allowed { assert_eq!(admissions.lock().expect("admission")[0]["permission"], "read"); }
    let blocked = messages.iter().find_map(|message| match message {
        maho_agent::types::AgentMessage::Llm(maho_ai::types::Message::ToolResult(result)) => Some(result), _ => None,
    }).expect("actual provider tool result");
    assert_eq!(blocked.is_error, !allowed);
    if allowed {
        assert_eq!(maho_ai::utils::text::content_text(&blocked.content, ""), "authorized execution");
    } else {
        assert!(maho_ai::utils::text::content_text(&blocked.content, "").contains("Permission required"));
    }
    }
}
