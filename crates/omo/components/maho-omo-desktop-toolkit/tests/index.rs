use maho_ext_api::{
    EventBus, Extension, ExtensionApi, ExtensionContext, ExtensionMode, ExtensionRuntime,
    ExtensionSessionProfile, ExtensionUi, ExtensionUiDialogOptions, JsonValue, LoadedExtension,
    Model, ModelRegistry, NotificationType, SessionEntry, SessionManager, SourceInfo, Theme,
    ToolSessionManager, UiFuture,
};
use maho_omo_desktop_toolkit::*;
use serde_json::json;
use std::sync::{Arc, Mutex};

struct TestSession(String);
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str {
        &self.0
    }
    fn session_file(&self) -> Option<&std::path::Path> {
        None
    }
}
impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}

struct TestModels;
impl ModelRegistry for TestModels {
    fn get_all(&self) -> Vec<Model> {
        Vec::new()
    }
    fn get_available(&self) -> Vec<Model> {
        Vec::new()
    }
    fn find(&self, _: &str, _: &str) -> Option<Model> {
        None
    }
    fn has_configured_auth(&self, _: &Model) -> bool {
        false
    }
    fn get_api_key_for_provider<'a>(
        &'a self,
        _: &'a str,
    ) -> maho_ext_api::ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Default)]
struct TestUi;
impl ExtensionUi for TestUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(
        &self,
        _: &str,
        _: Option<maho_ext_api::WidgetContent>,
        _: maho_ext_api::ExtensionWidgetOptions,
    ) {
    }
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(
        &self,
        _: maho_ext_api::ComponentFactory,
        _: maho_ext_api::CustomUiOptions,
    ) -> maho_ext_api::ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Ok(JsonValue::Null) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

fn test_context(session_id: &str) -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(TestUi),
        mode: ExtensionMode::Tui,
        has_ui: true,
        cwd: std::path::PathBuf::from("/workspace"),
        agent_dir: std::path::PathBuf::from("/workspace/.omo"),
        session_manager: Arc::new(TestSession(session_id.to_string())),
        model_registry: Arc::new(TestModels),
        model: None,
        thinking_level: None,
        service_tier: None,
        effective_service_tier: None,
        scoped_models: Vec::new(),
        goal_store_file: None,
        loaded_extension_paths: Vec::new(),
        signal: None,
        steering_signal: None,
        is_idle_fn: Arc::new(|| true),
        wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
        is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(String::new),
        get_system_prompt_options_fn: Arc::new(maho_ext_api::BuildSystemPromptOptions::default),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        logger: None,
        idle_coordinator: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

fn setup(offered: &str, timeout_ms: u64) -> (ExtensionApi, ExtensionRuntime, ExtensionContext) {
    let runtime = ExtensionRuntime::default();
    let mut profile = ExtensionSessionProfile::default();
    profile
        .session_context
        .insert(TOOLKITS_CONTEXT_KEY.into(), offered.into());
    let mut api = ExtensionApi::new(
        LoadedExtension::new(
            "desktop-toolkit",
            "/workspace".into(),
            SourceInfo::default(),
        ),
        profile,
        EventBus::default(),
        runtime.clone(),
    );
    create_desktop_toolkit_component(Some(timeout_ms)).register(&mut api);
    (api, runtime, test_context("test"))
}
#[tokio::test]
async fn acquisition_grants_refusals_validation_and_late_replies() {
    for response in [
        json!({"grant":{"endpoint":"http://127.0.0.1:41234/toolkit/browser","tokenFile":"/tmp/browser.token"}}),
        json!({"refusal":{"reason":"app_window_closed"}}),
        json!({"grant":{"endpoint":"http://example.com","tokenFile":"/tmp/t"}}),
    ] {
        let (api, runtime, context) = setup("browser", 1000);
        let handlers = runtime.live_rpc_handlers("desktop-toolkit").unwrap();
        let grant = handlers[TOOLKIT_GRANT_RPC].clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        let send = Arc::new(Mutex::new(Some(send)));
        let subscription = api.events.on(
            "senpi:extension-rpc-event",
            Arc::new(move |event| {
                if let Some(sender) = send.lock().unwrap().take() {
                    let _ = sender.send(event.clone());
                }
            }),
        );
        let execute = runtime
            .extension_tool_executor("desktop-toolkit", DESKTOP_TOOLKIT_TOOL_NAME)
            .unwrap();
        let operation = execute(
            "call",
            json!({"op":"acquire","toolkit":"browser"}),
            None,
            None,
            &context,
        );
        let request = async {
            let event: JsonValue = tokio::time::timeout(std::time::Duration::from_secs(1), receive)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(event["name"], TOOLKIT_REQUEST_EVENT);
            let mut message = response.clone();
            message["requestId"] = event["data"]["requestId"].clone();
            assert_eq!(
                grant(message.clone()).await.unwrap(),
                json!({"accepted":true})
            );
            assert_eq!(grant(message).await.unwrap(), json!({"accepted":false}));
        };
        let (result, _) = tokio::join!(operation, request);
        let details = result.unwrap().details;
        if response.get("refusal").is_some() {
            assert_eq!(details, json!({"ok":false,"reason":"app_window_closed"}));
        } else if response["grant"]["endpoint"] == "http://example.com" {
            assert_eq!(details, json!({"ok":false,"reason":"invalid_grant"}));
        } else {
            assert_eq!(details, json!({"ok":true,"grant":response["grant"]}));
        }
        drop(subscription);
    }
}
#[tokio::test(start_paused = true)]
async fn timeout_and_unoffered_toolkit() {
    for (offered, reason) in [("", "unavailable"), ("browser", "timeout")] {
        let (_api, runtime, context) = setup(offered, 20);
        let execute = runtime
            .extension_tool_executor("desktop-toolkit", DESKTOP_TOOLKIT_TOOL_NAME)
            .unwrap();
        let result = execute(
            "call",
            json!({"op":"acquire","toolkit":"browser"}),
            None,
            None,
            &context,
        )
        .await
        .unwrap();
        assert_eq!(result.details, json!({"ok":false,"reason":reason}));
    }
}
