#[tokio::test]
async fn sdk_native_factory_discovers_resources_and_recreates_on_new_session() {
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    use maho_core::{sdk::{create_agent_session, CreateAgentSessionOptions}, session_manager::SessionManager};
    let dir = tempfile::tempdir().expect("isolated SDK");
    let cwd = dir.path().to_string_lossy().into_owned();
    let prompt = dir.path().join("sdk_fixture.md");
    std::fs::write(&prompt, "---\nname: sdk_fixture\ndescription: fixture\n---\nSDK_RESOURCE $1").expect("prompt");
    let skill = dir.path().join("SKILL.md");
    std::fs::write(&skill, "---\nname: sdk-skill\ndescription: SDK skill fixture\n---\nRead the fixture.").expect("skill");
    let created = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(AtomicUsize::new(0));
    let admitted = Arc::new(AtomicUsize::new(0));
    let (admission_tx, mut admission_rx) = tokio::sync::mpsc::unbounded_channel();
    let factory_created = created.clone();
    let factory_started = started.clone();
    let factory_admitted = admitted.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "sdk-resource-fixture".to_owned(), source_info: Default::default(),
        factory: Arc::new(move |api| {
            factory_created.fetch_add(1, Ordering::SeqCst);
            let started = factory_started.clone();
            let prompt = prompt.clone();
            let skill = skill.clone();
            let admitted = factory_admitted.clone();
            let admission_tx = admission_tx.clone();
            let actions = Arc::new(maho_ext_api::ExtensionApi::new(
                maho_ext_api::LoadedExtension::new("sdk-resource-fixture", api.cwd.clone(), Default::default()),
                api.profile.clone(), api.events.clone(), api.runtime.clone()));
            Box::pin(async move {
                api.register_tool(maho_ext_api::ToolDefinition::new("sdk_registered_fixture", "Execute native SDK fixture",
                    serde_json::json!({"type":"object","properties":{}}), Arc::new(|_| Box::pin(async {
                        Ok(maho_ext_api::ToolResult { content: vec![maho_ext_api::ToolContent::Text {
                            text: "SDK_REGISTERED_EXECUTED".into(), audience: None,
                        }], details: None })
                    }))));
                api.on(maho_ext_api::EventKind::SessionStart, Arc::new(move |_, _| {
                    let started = started.clone();
                    let actions = actions.clone();
                    Box::pin(async move {
                        started.fetch_add(1, Ordering::SeqCst);
                        actions.send_user_message(maho_ext_api::UserMessageContent::Text("sdk startup".into()), Default::default())?;
                        Ok(maho_ext_api::EventResult::None)
                    })
                }));
                api.on(maho_ext_api::EventKind::Input, Arc::new(move |_, _| {
                    admitted.fetch_add(1, Ordering::SeqCst);
                    admission_tx.send(()).expect("admission observer");
                    Box::pin(async { Ok(maho_ext_api::EventResult::Input(maho_ext_api::InputEventResult::Handled)) })
                }));
                api.on(maho_ext_api::EventKind::ResourcesDiscover, Arc::new(move |_, _| {
                    let prompt = prompt.clone();
                    let skill = skill.clone();
                    Box::pin(async move { Ok(maho_ext_api::EventResult::ResourcesDiscover(maho_ext_api::ResourcesDiscoverResult {
                        prompt_paths: vec![prompt.to_string_lossy().into_owned().into()],
                        skill_paths: vec![skill.to_string_lossy().into_owned().into()], ..Default::default()
                    })) })
                }));
                Ok(())
            })
        }),
    };
    let model = serde_json::from_value(serde_json::json!({
        "id":"fixture","name":"fixture","api":"faux","provider":"faux","baseUrl":"",
        "reasoning":false,"input":[],"contextWindow":128000,"maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
    })).expect("model");
    let session = tokio::time::timeout(std::time::Duration::from_secs(5), create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        model: Some(model), session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
        extension_factories: vec![factory], ..Default::default()
    })).await.expect("bounded native SDK startup").expect("SDK session").session;
    let bound = session.extension_runner_bound().await;
    let initial_starts = started.load(Ordering::SeqCst);
    let initial_resources = session.prompt_templates();
    let lifecycle = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        admission_rx.recv().await.ok_or("initial admission observer closed")?;
        let initial_admissions = admitted.load(Ordering::SeqCst);
        let first = session.execute_tool("sdk_registered_fixture", serde_json::json!({}), Default::default()).await
            .map_err(|error| error.message)?;
        session.new_session(None).await?;
        admission_rx.recv().await.ok_or("replacement admission observer closed")?;
        let second = session.execute_tool("sdk_registered_fixture", serde_json::json!({}), Default::default()).await
            .map_err(|error| error.message)?;
        Ok::<_, String>((first, second, initial_admissions))
    }).await;
    let resources = session.prompt_templates();
    let creations = created.load(Ordering::SeqCst);
    let starts = started.load(Ordering::SeqCst);
    let skills = session.extension_context_actions().get_system_prompt_options().skills;
    let admissions = admitted.load(Ordering::SeqCst);
    session.dispose().await;
    assert!(bound);
    assert_eq!(initial_starts, 1);
    let (first, second, initial_admissions) = lifecycle.expect("bounded SDK lifecycle").expect("replacement admission and execution");
    assert_eq!(initial_admissions, 1, "initial startup input admitted before replacement");
    for result in [first, second] {
        assert_eq!(maho_ai::utils::text::content_text(&result.content, ""), "SDK_REGISTERED_EXECUTED");
    }
    assert_eq!(maho_core::prompt_templates::expand_prompt_template("/sdk_fixture first", &initial_resources), "SDK_RESOURCE first");
    assert_eq!(creations, 2);
    assert_eq!(starts, 2);
    assert_eq!(admissions, 2);
    assert_eq!(skills.iter().filter(|skill| skill.name == "sdk-skill").count(), 1);
    assert_eq!(skills.iter().find(|skill| skill.name == "sdk-skill").expect("extension skill").file_path,
        dir.path().join("SKILL.md").to_string_lossy());
    assert_eq!(maho_core::prompt_templates::expand_prompt_template("/sdk_fixture second", &resources), "SDK_RESOURCE second");
}

#[tokio::test]
async fn reported_native_factory_failure_keeps_session_disposal_effective() {
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    let dir = tempfile::tempdir().expect("isolated failed SDK");
    let cwd = dir.path().to_string_lossy().into_owned();
    let manager = maho_core::session_manager::SessionManager::in_memory(&cwd, None, None);
    let id = manager.session_id().to_owned();
    let cleanups = Arc::new(AtomicUsize::new(0));
    let observed = cleanups.clone();
    let unregister = maho_ai::session_resources::register_session_resource_cleanup(Arc::new(move |session_id| {
        if session_id == Some(id.as_str()) { observed.fetch_add(1, Ordering::SeqCst); }
        Ok(())
    }));
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "sdk-failed-factory".into(), source_info: Default::default(),
        factory: Arc::new(|_| Box::pin(async { Err("native factory refused".into()) })),
    };
    let model = serde_json::from_value(serde_json::json!({
        "id":"fixture","name":"fixture","api":"faux","provider":"faux","baseUrl":"",
        "reasoning":false,"input":[],"contextWindow":128000,"maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
    })).expect("model");
    let result = tokio::time::timeout(std::time::Duration::from_secs(5),
        maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            model: Some(model), session_manager: Some(manager), tools: Some(Vec::new()),
            extension_factories: vec![factory], ..Default::default()
        })).await;
    let before_disposal = cleanups.load(Ordering::SeqCst);
    if let Ok(Ok(result)) = &result { result.session.dispose().await; }
    unregister();
    let result = result.expect("bounded factory error reporting");
    assert!(result.is_ok(), "native loader reports extension errors without rejecting the session");
    assert_eq!(before_disposal, 0);
    assert_eq!(cleanups.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelled_sdk_construction_retires_session_resources() {
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    for during_startup in [false, true] {
        let dir = tempfile::tempdir().expect("isolated cancelled SDK");
        let cwd = dir.path().to_string_lossy().into_owned();
        let manager = maho_core::session_manager::SessionManager::in_memory(&cwd, None, None);
        let id = manager.session_id().to_owned();
        let cleanups = Arc::new(AtomicUsize::new(0));
        let observed = cleanups.clone();
        let (cleanup_tx, mut cleanup_rx) = tokio::sync::mpsc::unbounded_channel();
        let unregister = maho_ai::session_resources::register_session_resource_cleanup(Arc::new(move |session_id| {
            if session_id == Some(id.as_str()) {
                observed.fetch_add(1, Ordering::SeqCst);
                cleanup_tx.send(()).expect("cleanup observer");
            }
            Ok(())
        }));
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
            path: "sdk-cancelled-factory".into(), source_info: Default::default(),
            factory: Arc::new(move |api| {
                let entered_tx = entered_tx.clone();
                Box::pin(async move {
                    if during_startup {
                        api.on(maho_ext_api::EventKind::SessionStart, Arc::new(move |_, _| {
                            let entered_tx = entered_tx.clone();
                            Box::pin(async move {
                                entered_tx.send(()).expect("startup entry observer");
                                std::future::pending::<Result<maho_ext_api::EventResult, maho_ext_api::ExtensionFailure>>().await
                            })
                        }));
                        return Ok(());
                    }
                    entered_tx.send(()).expect("factory entry observer");
                    std::future::pending::<Result<(), maho_ext_api::ExtensionFailure>>().await
                })
            }),
        };
        let model = serde_json::from_value(serde_json::json!({
            "id":"fixture","name":"fixture","api":"faux","provider":"faux","baseUrl":"",
            "reasoning":false,"input":[],"contextWindow":128000,"maxTokens":4096,
            "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
        })).expect("model");
        let mut construction = Box::pin(maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            model: Some(model), session_manager: Some(manager), tools: Some(Vec::new()),
            extension_factories: vec![factory], ..Default::default()
        }));
        let entered = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                entry = entered_rx.recv() => entry.is_some(),
                result = &mut construction => {
                    if let Ok(created) = result { created.session.dispose().await; }
                    false
                }
            }
        }).await;
        drop(construction);
        let cleanup = tokio::time::timeout(std::time::Duration::from_secs(5), cleanup_rx.recv()).await;
        unregister();
        assert!(entered.expect("bounded factory entry"));
        assert_eq!(cleanup.expect("bounded cancelled constructor cleanup"), Some(()));
        assert_eq!(cleanups.load(Ordering::SeqCst), 1);
    }
}
