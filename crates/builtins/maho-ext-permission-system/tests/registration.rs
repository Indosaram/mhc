use maho_ext_api::{
    EventBus, EventKind, EventResult, Extension, ExtensionApi, ExtensionContext, ExtensionEvent,
    ExtensionFailure, ExtensionMode, ExtensionRuntime, ExtensionSessionActions,
    ExtensionSessionProfile, ExtensionUi, ExtensionUiDialogOptions, FlagValue, JsonValue,
    LoadedExtension, Model, ModelRegistry, NotificationType, SessionEntry,
    SessionManager, SourceInfo, Theme, ToolCallEvent, ToolCallEventResult, ToolDefinition,
    ToolPermissionRequest, ToolResult, ToolSessionManager, UiFuture,
};
use maho_ext_permission_system::PermissionSystem;
use serde_json::json;
use std::sync::{Arc, Mutex};

#[test]
fn registers_policy_flags_and_lifecycle() {
    let mut api = ExtensionApi::new(
        LoadedExtension::new("permission", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    PermissionSystem.register(&mut api);
    assert_eq!(
        api.registered.flags.iter().map(|flag| flag.name.as_str()).collect::<Vec<_>>(),
        vec!["permission", "permission-preset"]
    );
    for kind in [EventKind::SessionStart, EventKind::ToolCall, EventKind::SessionShutdown] {
        assert_eq!(api.registered.handlers[&kind].len(), 1);
    }
}

struct TestSession(String);
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str { &self.0 }
    fn session_file(&self) -> Option<&std::path::Path> { None }
}
impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_branch(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
}

struct TestModels;
impl ModelRegistry for TestModels {
    fn get_all(&self) -> Vec<Model> { Vec::new() }
    fn get_available(&self) -> Vec<Model> { Vec::new() }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None }
    fn has_configured_auth(&self, _: &Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> maho_ext_api::ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(None) }) }
}

struct TestUi;
impl ExtensionUi for TestUi {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<maho_ext_api::WidgetContent>, _: maho_ext_api::ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: maho_ext_api::ComponentFactory, _: maho_ext_api::CustomUiOptions) -> maho_ext_api::ExtensionFuture<'_, JsonValue> { Box::pin(async { Ok(JsonValue::Null) }) }
    fn theme(&self) -> Theme { Theme::default() }
}

struct TestSessionActions {
    active: Mutex<Vec<String>>,
}
impl ExtensionSessionActions for TestSessionActions {
    fn set_session_name(&self, _: &str) -> Result<(), ExtensionFailure> { panic!("unexpected session rename") }
    fn get_session_name(&self) -> Result<Option<String>, ExtensionFailure> { panic!("unexpected session name") }
    fn set_label(&self, _: &str, _: Option<&str>) -> Result<(), ExtensionFailure> { panic!("unexpected label") }
    fn execute_tool<'a>(&'a self, _: &'a str, _: JsonValue, _: maho_ext_api::ExecuteToolOptions) -> maho_ext_api::ExecuteToolFuture<'a> { panic!("unexpected execution") }
    fn refresh_tools(&self) -> Result<(), ExtensionFailure> { Ok(()) }
    fn register_removed_tool_hint(&self, _: &str, _: &str) -> Result<(), ExtensionFailure> { panic!("unexpected hint") }
    fn register_lazy_tool_activator(&self, _: maho_ext_api::LazyToolActivator) -> Result<(), ExtensionFailure> { panic!("unexpected activator") }
    fn get_commands(&self) -> Result<Vec<maho_ext_api::SlashCommandInfo>, ExtensionFailure> { panic!("unexpected commands") }
    fn set_model(&self, _: Model) -> maho_ext_api::ExtensionFuture<'_, bool> { panic!("unexpected model") }
    fn get_thinking_level(&self) -> Result<maho_ext_api::ThinkingLevel, ExtensionFailure> { panic!("unexpected thinking") }
    fn set_thinking_level(&self, _: maho_ext_api::ThinkingLevel) -> Result<(), ExtensionFailure> { panic!("unexpected thinking") }
    fn set_session_model(&self, _: Model) -> maho_ext_api::ExtensionFuture<'_, bool> { panic!("unexpected model") }
    fn set_session_thinking_level(&self, _: maho_ext_api::ThinkingLevel) -> Result<(), ExtensionFailure> { panic!("unexpected thinking") }
    fn set_session_fast_mode(&self, _: bool) -> Result<(), ExtensionFailure> { panic!("unexpected fast mode") }
    fn exec<'a>(&'a self, _: &'a str, _: &'a [String], _: &'a std::path::Path, _: maho_ext_api::ExecOptions) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::ExecResult> { panic!("unexpected process") }
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> {
        Ok(self.active.lock().expect("active tools lock").clone())
    }
    fn set_active_tools(&self, names: Vec<String>) -> Result<(), ExtensionFailure> {
        *self.active.lock().expect("active tools lock") = names;
        Ok(())
    }
}

#[tokio::test]
async fn custom_extension_permission_parser_integration_tool_call() {
    let project_dir = tempfile::tempdir().expect("tempdir");
    let ctx = ExtensionContext {
        ui: Arc::new(TestUi),
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: project_dir.path().to_path_buf(),
        agent_dir: project_dir.path().join(".maho"),
        session_manager: Arc::new(TestSession("s1".into())),
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
    };

    let runtime = ExtensionRuntime::default();
    let actions = Arc::new(TestSessionActions {
        active: Mutex::new(vec!["omo_browser_bridge".into(), "dynamic_tool".into()]),
    });
    runtime.bind_session_actions(actions);

    let scoped_runtime = runtime.registration_scope();
    let mut api = ExtensionApi::new(
        LoadedExtension::new("test-perm", project_dir.path().to_path_buf(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        scoped_runtime.clone(),
    );
    PermissionSystem.register(&mut api);
    scoped_runtime.set_flag("permission-preset", FlagValue::String("ask".into()));

    let mut bb_def = ToolDefinition::new(
        "omo_browser_bridge",
        "Browser bridge",
        json!({"type": "object"}),
        Arc::new(|_| Box::pin(async { Ok(ToolResult::text("ok")) })),
    );
    bb_def.permission_parser = Some(Arc::new(|_input, _cwd| Vec::new()));
    api.try_register_tool(bb_def).expect("register bridge tool");

    let mut dynamic_def = ToolDefinition::new(
        "dynamic_tool",
        "Dynamic tool",
        json!({"type": "object"}),
        Arc::new(|_| Box::pin(async { Ok(ToolResult::text("ok")) })),
    );
    dynamic_def.permission_parser = Some(Arc::new(|input, _cwd| {
        if input.get("allow").and_then(serde_json::Value::as_bool) == Some(true) {
            Vec::new()
        } else {
            vec![ToolPermissionRequest::new("dynamic_perm", vec!["item".into()], vec!["item/*".into()])]
        }
    }));
    api.try_register_tool(dynamic_def).expect("register dynamic tool");

    scoped_runtime.commit_registration().expect("commit registration succeeds");

    let session_start_handlers = api.registered.handlers[&EventKind::SessionStart].clone();
    let mut start_event = ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent { reason: maho_ext_api::SessionReason::Startup, initial_model_provenance: None, previous_session_file: None });
    for h in session_start_handlers {
        h(&mut start_event, &ctx).await.expect("start ok");
    }

    let tool_call_handlers = api.registered.handlers[&EventKind::ToolCall].clone();

    let mut bb_call = ExtensionEvent::ToolCall(ToolCallEvent {
        tool_name: "omo_browser_bridge".into(),
        tool_call_id: "c1".into(),
        input: json!({"op": "state"}),
    });
    let bb_res = tool_call_handlers[0](&mut bb_call, &ctx).await.expect("bb call ok");
    assert!(matches!(bb_res, EventResult::None));

    let mut dynamic_deny = ExtensionEvent::ToolCall(ToolCallEvent {
        tool_name: "dynamic_tool".into(),
        tool_call_id: "c2".into(),
        input: json!({"allow": false}),
    });
    let deny_res = tool_call_handlers[0](&mut dynamic_deny, &ctx).await.expect("deny call ok");
    assert!(matches!(deny_res, EventResult::ToolCall(ToolCallEventResult { block: Some(true), .. })));

    let mut dynamic_allow = ExtensionEvent::ToolCall(ToolCallEvent {
        tool_name: "dynamic_tool".into(),
        tool_call_id: "c3".into(),
        input: json!({"allow": true}),
    });
    let allow_res = tool_call_handlers[0](&mut dynamic_allow, &ctx).await.expect("allow call ok");
    assert!(matches!(allow_res, EventResult::None));

    let mut unknown_call = ExtensionEvent::ToolCall(ToolCallEvent {
        tool_name: "unknown_tool".into(),
        tool_call_id: "c4".into(),
        input: json!({}),
    });
    let unknown_res = tool_call_handlers[0](&mut unknown_call, &ctx).await.expect("unknown call ok");
    assert!(matches!(unknown_res, EventResult::ToolCall(ToolCallEventResult { block: Some(true), .. })));

    // Reload: re-register dynamic_tool with parser that now returns empty requests (allow) even when allow: false
    let mut reload_def = ToolDefinition::new(
        "dynamic_tool",
        "Dynamic tool reloaded",
        json!({"type": "object"}),
        Arc::new(|_| Box::pin(async { Ok(ToolResult::text("ok")) })),
    );
    reload_def.permission_parser = Some(Arc::new(|_input, _cwd| Vec::new()));
    api.try_register_tool(reload_def).expect("re-register dynamic tool");

    let mut reloaded_call = ExtensionEvent::ToolCall(ToolCallEvent {
        tool_name: "dynamic_tool".into(),
        tool_call_id: "c5".into(),
        input: json!({"allow": false}),
    });
    let reloaded_res = tool_call_handlers[0](&mut reloaded_call, &ctx).await.expect("reloaded call ok");
    assert!(matches!(reloaded_res, EventResult::None));
}
