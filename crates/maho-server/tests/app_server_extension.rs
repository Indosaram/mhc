use maho_ext_api::*;
use maho_ext_host::*;
use std::{path::Path, sync::Arc};

struct TestSession;
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str {
        "session"
    }
    fn session_file(&self) -> Option<&Path> {
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
struct TestRegistry;
impl ModelRegistry for TestRegistry {
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
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}
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
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("UI not available".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}
fn context() -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(TestUi),
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: "/tmp".into(),
        agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(TestSession),
        model_registry: Arc::new(TestRegistry),
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
        get_system_prompt_fn: Arc::new(|| "base".into()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions {
            cwd: "/tmp".into(),
            ..Default::default()
        }),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None, compaction_signal: Default::default(),
    }
}

#[tokio::test]
async fn extension_request_uses_real_registered_runner() {
    use maho_server::app_server::registry::{
        JsonRpcError, MethodRegistry, RegistryConnection, register_extension_request_method,
    };
    let runtime = ExtensionRuntime::default();
    let events = EventBus::default();
    let mut api = ExtensionApi::new(
        LoadedExtension::new("echo", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        events.clone(),
        runtime.clone(),
    );
    api.rpc_handle("echo", Arc::new(|data| Box::pin(async move { Ok(data) })))
        .unwrap();
    let runner = Arc::new(ExtensionRunner::new(
        vec![api.registered],
        runtime,
        events,
        context(),
    ));
    let selected = runner.clone();
    let mut registry = MethodRegistry::default();
    register_extension_request_method(
        &mut registry,
        Arc::new(move |id| {
            if id == "thread" {
                Ok(selected.clone())
            } else {
                Err(JsonRpcError::new(-32603, "Thread not found"))
            }
        }),
    );
    let connection = RegistryConnection {
        initialized: true,
        experimental_api: false,
        ..Default::default()
    };
    let request = serde_json::json!({"id":1,"method":"extension_request","params":{"threadId":"thread","name":"echo","data":{"value":true}}});
    assert_eq!(
        registry.dispatch(connection.clone(), request.clone()).await["result"],
        serde_json::json!({"value":true})
    );
    assert_eq!(registry.dispatch(connection.clone(),serde_json::json!({"id":2,"method":"extension_request","params":{"threadId":"","name":"echo"}})).await["error"]["code"],-32602);
    runner.invalidate("replaced");
    assert_eq!(
        registry.dispatch(connection, request).await["error"]["code"],
        -32603
    );
}
