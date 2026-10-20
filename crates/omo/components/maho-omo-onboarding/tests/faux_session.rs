use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::load_script, faux_session::FauxSession};

#[tokio::test]
async fn faux_registered_component_preserves_idle_prompt_and_provider_reply() {
    let state = tempfile::tempdir().expect("private onboarding state");
    assert!(maho_omo_onboarding::state::claim_onboarding(state.path()));
    let script = load_script("hello").expect("shared faux script");
    let reference: serde_json::Value = serde_json::from_str(include_str!("reference-hello.json"))
        .expect("generated pinned reference");
    let session = FauxSession::new(script).with_extension("omo").with_native_extension(
        NativeExtensionFactory {
            path: "<onboarding>".into(),
            source_info: Default::default(),
            extension: Box::new(maho_omo_onboarding::OnboardingComponent { state_dir: state.path().into(), skills_root: "<skills>".into() }),
        },
    );
    let actual = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await.expect("bounded native session").expect("registered component run");
    let messages = actual["messages"].as_array().expect("native messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    let expected: Vec<_> = reference["entries"].as_array().expect("reference entries").iter()
        .filter_map(|entry| entry.get("message"))
        .filter(|message| matches!(message["role"].as_str(), Some("user" | "assistant")))
        .collect();
    assert_eq!(messages[0]["content"], expected[0]["content"]);
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"], expected[1]["content"]);
    assert!(actual["events"].as_array().expect("session events").iter()
        .any(|event| event["type"] == "agent_end"));
}

#[tokio::test]
async fn interactive_prompt_claims_without_bootstrap() {
    let root = tempfile::tempdir().expect("private state");
    struct TestUi;
    impl maho_ext_api::ExtensionUi for TestUi {
        fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, Option<String>> { Box::pin(async { None }) }
        fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, bool> { Box::pin(async { false }) }
        fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, Option<String>> { Box::pin(async { None }) }
        fn notify(&self, _: &str, _: maho_ext_api::NotificationType) {}
        fn set_status(&self, _: &str, _: Option<&str>) {}
        fn set_widget(&self, _: &str, _: Option<maho_ext_api::WidgetContent>, _: maho_ext_api::ExtensionWidgetOptions) {}
        fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
        fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
        fn set_title(&self, _: &str) {}
        fn paste_to_editor(&self, _: &str) {}
        fn set_editor_text(&self, _: &str) {}
        fn get_editor_text(&self) -> String { String::new() }
        fn custom(&self, _: maho_ext_api::ComponentFactory, _: maho_ext_api::CustomUiOptions) -> maho_ext_api::ExtensionFuture<'_, maho_ext_api::JsonValue> { Box::pin(async { Ok(maho_ext_api::JsonValue::Null) }) }
        fn theme(&self) -> maho_ext_api::Theme { maho_ext_api::Theme::default() }
    }

    use std::sync::Arc;
    use maho_ai::providers::faux::{faux_provider, RegisterFauxProviderOptions, faux_assistant_message, FauxAssistantMessageOptions};
    let cwd=root.path().to_string_lossy().into_owned();
    let provider=faux_provider(RegisterFauxProviderOptions { tokens_per_second:Some(0.0), ..Default::default() }).expect("valid faux provider");
    provider.set_responses(["Welcome.","Hello."].into_iter().map(|text| faux_assistant_message(text,FauxAssistantMessageOptions {timestamp:Some(0),..Default::default()}).into()).collect());
    let runtime=maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path:Some(root.path().join("models.json")),auth_path:Some(root.path().join("auth.json")),
        credentials:Some(Arc::new(maho_core::auth_storage::AuthStorage::in_memory([("faux".into(),serde_json::json!({"type":"api_key","key":"fixture"}))].into_iter().collect()))),
        providers:Some(vec![provider.provider.clone()]),
            ..Default::default()
        }).expect("valid model runtime");
    let loaded=maho_ext_host::loader::load_extensions(vec![NativeExtensionFactory {
        path:"<onboarding>".into(),source_info:Default::default(),extension:Box::new(maho_omo_onboarding::OnboardingComponent {state_dir:root.path().into(),skills_root:"<skills>".into()}),
    }],root.path(),Default::default());
    let session=maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd:Some(cwd.clone()),agent_dir:Some(root.path().join("agent").to_string_lossy().into_owned()),model_runtime:Some(runtime),model:provider.get_model(Some("faux-1")),
        session_manager:Some(maho_core::session_manager::SessionManager::in_memory(&cwd,None,None)),
        settings_manager:Some(maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),false)),
        loaded_extensions:Some(loaded),tools:Some(Vec::new()),auto_title_sessions:Some(false),defer_extension_start:true,..Default::default()
    }).await.expect("native session").session;
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),async {
        session.bind_extensions(maho_core::agent_session::ExtensionBindings {ui_context:Some(Arc::new(TestUi)),mode:Some(maho_ext_api::ExtensionMode::Tui),..Default::default()}).await;
        session.wait_for_idle().await;
        session.prompt("hi",Default::default()).await
    }).await;
    let messages:Vec<_>=session.messages().iter().map(maho_core::agent_session::session_message_to_value).collect::<Result<_,_>>().expect("flat messages");
    session.emit_session_shutdown(maho_ext_api::SessionReason::Quit).await;
    session.dispose().await;
    result.expect("bounded startup and prompt").expect("prompt");
    assert!(maho_omo_onboarding::state::is_onboarding_complete(root.path()));
    let bootstraps:Vec<_>=messages.iter().filter(|message|message["customType"]=="omo-onboarding:bootstrap").collect();
    assert!(bootstraps.is_empty());
    let assistants:Vec<_>=messages.iter().filter(|message|message["role"]=="assistant").collect();
    assert_eq!(assistants.len(),1);
    assert_eq!(assistants[0]["content"][0]["text"],"Welcome.");
}
