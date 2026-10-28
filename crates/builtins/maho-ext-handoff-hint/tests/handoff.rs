use maho_core::agent_session::{ExtensionBindings, PromptOptions};
use maho_core::sdk::{CreateAgentSessionOptions, HostRuntimeFactory};
use maho_ext_api::{EventKind, EventResult, Extension, ExtensionError, SessionBeforeSwitchResult};
use maho_ext_handoff_hint::index::{HANDOFF_BRIEF_MESSAGE_TYPE, HandoffHint};
use maho_ext_host::loader::NativeAsyncExtensionFactory;
use std::sync::{Arc, Mutex};

async fn fixture(
    cancel: bool,
) -> (
    tempfile::TempDir,
    maho_core::agent_session::AgentSession,
    Arc<Mutex<Vec<ExtensionError>>>,
) {
    let directory = tempfile::tempdir().expect("isolated session");
    let cwd = directory.path().to_string_lossy().into_owned();
    let errors = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&errors);
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(
        maho_core::model_runtime::CreateModelRuntimeOptions {
            providers: Some(Vec::new()),
            ..Default::default()
        },
    )
    .expect("model runtime");
    let host = HostRuntimeFactory {
        model_registry: maho_core::model_registry::ModelRegistry::new(runtime.clone()),
        model_runtime: runtime,
        extension_factories: vec![NativeAsyncExtensionFactory {
            path: "handoff-hint".into(),
            source_info: Default::default(),
            factory: Arc::new(move |api| {
                HandoffHint::default().register(api);
                if cancel {
                    api.on(
                        EventKind::SessionBeforeSwitch,
                        Arc::new(|_, _| {
                            Box::pin(async {
                                Ok(EventResult::SessionBefore(SessionBeforeSwitchResult {
                                    cancel: Some(true),
                                    ..Default::default()
                                }))
                            })
                        }),
                    );
                }
                Box::pin(async { Ok(()) })
            }),
        }],
    };
    let model = serde_json::from_value(serde_json::json!({
        "id":"fixture","name":"fixture","api":"faux","provider":"faux","baseUrl":"",
        "reasoning":false,"input":[],"contextWindow":128000,"maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
    }))
    .expect("model");
    let session = host
        .create(CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(
                directory
                    .path()
                    .join("agent")
                    .to_string_lossy()
                    .into_owned(),
            ),
            model: Some(model),
            tools: Some(Vec::new()),
            session_manager: Some(maho_core::session_manager::SessionManager::create(
                &cwd,
                Some(&directory.path().join("sessions").to_string_lossy()),
                None,
            )),
            extension_bindings: Some(Arc::new(move |_| {
                let captured = Arc::clone(&captured);
                ExtensionBindings {
                    on_error: Some(Arc::new(move |error| {
                        captured.lock().expect("errors").push(error.clone())
                    })),
                    ..Default::default()
                }
            })),
            ..Default::default()
        })
        .await
        .expect("session")
        .session;
    session.with_session_manager_mut(|manager| {
        manager.append_message(serde_json::json!({
            "role":"user", "content":"handoff-fixture-request", "timestamp":1
        }));
    });
    (directory, session, errors)
}

#[tokio::test]
async fn handoff_links_a_fresh_session_delivers_brief_and_starts_no_turn() {
    let (_directory, session, errors) = fixture(false).await;
    let original_id = session.session_id();
    let original_file = session.session_file().expect("persisted session");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        session.prompt("/handoff", PromptOptions::default()),
    )
    .await;
    let current_id = session.session_id();
    let (header, entries) =
        session.with_session_manager(|manager| (manager.header(), manager.entries()));
    let messages = session.messages();
    let pending = session.pending_message_count();
    let streaming = session.is_streaming();
    session.dispose().await;

    result.expect("bounded handoff").expect("command handled");
    assert_ne!(current_id, original_id);
    assert_eq!(header.expect("header")["parentSession"], original_file);
    let briefs: Vec<_> = entries
        .iter()
        .filter(|entry| entry["customType"] == HANDOFF_BRIEF_MESSAGE_TYPE)
        .collect();
    assert_eq!(briefs.len(), 1);
    assert_eq!(briefs[0]["display"], true);
    assert!(
        briefs[0]["content"]
            .to_string()
            .contains("handoff-fixture-request")
    );
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role(), "custom");
    assert_eq!(pending, 0);
    assert!(!streaming);
    assert!(
        errors.lock().expect("errors").is_empty(),
        "{:?}",
        errors.lock().expect("errors")
    );
}

#[tokio::test]
async fn cancelled_handoff_keeps_the_original_session_without_a_brief() {
    let (_directory, session, errors) = fixture(true).await;
    let original_id = session.session_id();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        session.prompt("/handoff", PromptOptions::default()),
    )
    .await;
    let current_id = session.session_id();
    let entries = session.with_session_manager(|manager| manager.entries());
    session.dispose().await;

    result
        .expect("bounded cancellation")
        .expect("command handled");
    assert_eq!(current_id, original_id);
    assert!(
        !entries
            .iter()
            .any(|entry| entry["customType"] == HANDOFF_BRIEF_MESSAGE_TYPE)
    );
    assert!(errors.lock().expect("errors").is_empty());
}
