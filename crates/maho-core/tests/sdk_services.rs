use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

#[tokio::test]
async fn services_mount_preserves_settings_and_consumes_loaded_factory_once() {
    let dir = tempfile::tempdir().expect("isolated services");
    let cwd = dir.path().to_string_lossy().into_owned();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "loaded-once".into(), source_info: Default::default(),
        factory: Arc::new(move |_| { counted.fetch_add(1, Ordering::SeqCst); Box::pin(async { Ok(()) }) }),
    };
    let loaded = maho_ext_host::loader::load_extensions_async(vec![factory.clone()], dir.path(), Default::default()).await;
    let mut services = maho_core::agent_session_services::create_agent_session_services(
        maho_core::agent_session_services::CreateAgentSessionServicesOptions {
            cwd: cwd.clone(), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            model_runtime: Some(maho_core::model_runtime::ModelRuntime::create_sync(
                maho_core::model_runtime::CreateModelRuntimeOptions { providers: Some(Vec::new()), ..Default::default() })),
            ..Default::default()
        }).await;
    services.settings_manager.apply_overrides(&serde_json::Map::from_iter([("fixtureSetting".into(), 7.into())]));
    let model = serde_json::from_value(serde_json::json!({"id":"fixture","name":"fixture","api":"faux","provider":"faux",
        "baseUrl":"","reasoning":false,"input":[],"contextWindow":128000,"maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}})).expect("model");
    let created = maho_core::agent_session_services::create_agent_session_from_services(services,
        maho_core::agent_session_services::CreateAgentSessionFromServicesOptions {
            model: Some(model), loaded_extensions: Some(loaded), extension_factories: vec![factory],
            session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
            ..Default::default()
        }).await.expect("mount");
    let mounted_value = created.session.with_settings_manager(|manager| manager.get_value("fixtureSetting").cloned());
    created.services.settings_manager.lock().expect("shared settings").apply_overrides(
        &serde_json::Map::from_iter([("fixtureSetting".into(), 9.into())]));
    let live_value = created.session.with_settings_manager(|manager| manager.get_value("fixtureSetting").cloned());
    created.session.dispose().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(mounted_value, Some(7.into()));
    assert_eq!(live_value, Some(9.into()));
}
