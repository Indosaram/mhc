use std::sync::{Arc, Mutex};

#[tokio::test]
async fn native_session_switch_exposes_overridden_cwd_to_recreated_factory() {
    let directory = tempfile::tempdir().expect("isolated SDK");
    let original = directory.path().join("original");
    let replacement = directory.path().join("replacement");
    std::fs::create_dir(&original).expect("original cwd");
    std::fs::create_dir(&replacement).expect("replacement cwd");
    let original = original.to_string_lossy().into_owned();
    let replacement = replacement.to_string_lossy().into_owned();
    let session_path = directory.path().join("resume.jsonl");
    std::fs::write(&session_path, format!("{}\n", serde_json::json!({
        "type":"session","version":3,"id":"rpc-resume-fixture","timestamp":"2026-01-01T00:00:00Z","cwd":original
    }))).expect("session header");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "rpc-cwd-fixture".into(), source_info: Default::default(),
        factory: Arc::new(move |api| {
            let seen = captured.clone();
            api.on(maho_ext_api::EventKind::SessionStart, Arc::new(move |_, context| {
                seen.lock().expect("startup cwd").push(context.cwd.clone());
                Box::pin(async { Ok(maho_ext_api::EventResult::None) })
            }));
            Box::pin(async { Ok(()) })
        }),
    };
    let model = serde_json::from_value(serde_json::json!({
        "id":"fixture","name":"fixture","api":"faux","provider":"faux","baseUrl":"",
        "reasoning":false,"input":[],"contextWindow":128000,"maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
    })).expect("model");
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        providers: Some(Vec::new()), ..Default::default()
    });
    let shared = runtime.credentials.clone();
    let host = maho_core::sdk::HostRuntimeFactory { model_registry: maho_core::model_registry::ModelRegistry::new(runtime.clone()),
        model_runtime: runtime, extension_factories: vec![factory] };
    let session = host.create(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(original.clone()), agent_dir: Some(directory.path().join("agent").to_string_lossy().into_owned()),
        model: Some(model), tools: Some(Vec::new()),
        session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&original, None, None)),
        ..Default::default()
    }).await.expect("SDK session").session;
    let switched = tokio::time::timeout(std::time::Duration::from_secs(5),
        session.switch_session_with_cwd(&session_path.to_string_lossy(), Some(&replacement))).await;
    let cwd = session.cwd();
    let manager_cwd = session.with_session_manager(|manager| manager.cwd().to_owned());
    let session_id = session.session_id();
    let same_store = Arc::ptr_eq(&shared, &session.model_runtime().credentials);
    session.dispose().await;
    assert!(switched.expect("bounded switch").expect("switch"));
    assert_eq!(cwd, replacement);
    assert_eq!(manager_cwd, replacement);
    assert_eq!(session_id, "rpc-resume-fixture");
    assert!(same_store);
    assert_eq!(seen.lock().expect("startup cwd").last(), Some(&std::path::PathBuf::from(replacement)));
}
