use std::sync::{Arc, Mutex};
use maho_ext_api::{
    EventBus, EventKind, Extension, ExtensionApi, ExtensionContext, ExtensionEvent,
    ExtensionFailure, ExtensionMode, ExtensionRuntime, ExtensionSessionProfile,
    ExtensionUi, ExtensionUiDialogOptions, InputEvent, InputSource, JsonValue,
    LoadedExtension, Model, ModelRegistry, NotificationType, SessionEntry,
    SessionManager, SourceInfo, Theme, ToolExecutionMode, ToolExposure, ToolSessionManager,
    UiFuture,
};
use maho_omo_browser_bridge::{
    BROWSER_BRIDGE_TOOL_NAME, BROWSER_STATE_EVENT, BROWSER_STOP_RPC,
    create_browser_bridge_component,
};
use serde_json::json;

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
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> maho_ext_api::ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Default)]
struct TestUi;
impl ExtensionUi for TestUi {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<maho_ext_api::WidgetContent>, _: maho_ext_api::ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: maho_ext_api::ComponentFactory, _: maho_ext_api::CustomUiOptions) -> maho_ext_api::ExtensionFuture<'_, JsonValue> {
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

struct TestFixture {
    api: ExtensionApi,
    runtime: ExtensionRuntime,
    _rpc_subscription: maho_ext_api::BusSubscription,
    rpc_events: Arc<Mutex<Vec<JsonValue>>>,
    context: ExtensionContext,
}

fn setup() -> TestFixture {
    setup_with_session("s1")
}

fn setup_with_session(session_id: &str) -> TestFixture {
    let runtime = ExtensionRuntime::default();
    let events = EventBus::default();
    let rpc_events = Arc::new(Mutex::new(Vec::new()));
    let rpc_events_sink = rpc_events.clone();

    let rpc_subscription = events.on(
        "senpi:extension-rpc-event",
        Arc::new(move |payload| {
            rpc_events_sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(payload.clone());
        }),
    );

    let mut api = ExtensionApi::new(
        LoadedExtension::new("browser-bridge", "/workspace".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        events,
        runtime.clone(),
    );

    let component = create_browser_bridge_component();
    component.register(&mut api);

    let context = test_context(session_id);

    TestFixture {
        api,
        runtime,
        _rpc_subscription: rpc_subscription,
        rpc_events,
        context,
    }
}

async fn execute_tool(fixture: &TestFixture, call_id: &str, params: JsonValue) -> maho_ext_api::AgentToolResult {
    let executor = fixture
        .runtime
        .extension_tool_executor("browser-bridge", BROWSER_BRIDGE_TOOL_NAME)
        .expect("registered tool executor");
    executor(call_id, params, None, None, &fixture.context)
        .await
        .expect("successful tool execution")
}

async fn invoke_rpc(fixture: &TestFixture, name: &str, params: JsonValue) -> Result<JsonValue, ExtensionFailure> {
    let handlers = fixture
        .runtime
        .live_rpc_handlers("browser-bridge")
        .expect("live rpc handlers");
    let handler = handlers.get(name).unwrap_or_else(|| panic!("missing RPC handler: {name}"));
    handler(params).await
}

async fn dispatch_input(fixture: &mut TestFixture, event: InputEvent) {
    let handlers = fixture
        .runtime
        .live_handlers("browser-bridge", EventKind::Input)
        .unwrap_or_default();
    let mut mut_event = ExtensionEvent::Input(event);
    for h in handlers {
        h(&mut mut_event, &fixture.context).await.expect("input handled");
    }
}

#[tokio::test]
async fn reports_state_and_emits_rpc_event() {
    let fixture = setup();
    let result = execute_tool(
        &fixture,
        "c1",
        json!({
            "op": "state",
            "data": { "engine": "connected", "status": "acting", "action": "click" }
        }),
    )
    .await;

    assert_eq!(result.details["ok"], true);
    assert_eq!(result.details["stopped"], false);

    let events = fixture.rpc_events.lock().unwrap().clone();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["name"], BROWSER_STATE_EVENT);
    assert_eq!(
        events[0]["data"],
        json!({ "engine": "connected", "status": "acting", "action": "click" })
    );
}

#[tokio::test]
async fn malformed_state_publishes_nothing() {
    let fixture = setup();

    let result1 = execute_tool(
        &fixture,
        "c1",
        json!({
            "op": "state",
            "data": { "status": "acting" }
        }),
    )
    .await;
    assert_eq!(result1.details["ok"], false);
    assert_eq!(result1.details["reason"], "invalid_state");

    let result2 = execute_tool(
        &fixture,
        "c2",
        json!({ "op": "state" }),
    )
    .await;
    assert_eq!(result2.details["ok"], false);
    assert_eq!(result2.details["reason"], "invalid_state");

    let events = fixture.rpc_events.lock().unwrap().clone();
    assert_eq!(events.len(), 0);
}

#[tokio::test]
async fn app_presses_stop_held_until_interactive_input() {
    let mut fixture = setup();

    let r1 = execute_tool(&fixture, "c1", json!({ "op": "status" })).await;
    assert_eq!(r1.details["stopped"], false);

    let stop_result = invoke_rpc(&fixture, BROWSER_STOP_RPC, json!({})).await.expect("stop rpc");
    assert_eq!(stop_result["stopped"], true);

    let r2 = execute_tool(&fixture, "c2", json!({ "op": "status" })).await;
    assert_eq!(r2.details["stopped"], true);

    dispatch_input(
        &mut fixture,
        InputEvent {
            input_id: "i1".to_string(),
            text: "wake".to_string(),
            images: None,
            source: InputSource::Extension,
            streaming_behavior: None,
        },
    )
    .await;

    let r3 = execute_tool(&fixture, "c3", json!({ "op": "status" })).await;
    assert_eq!(r3.details["stopped"], true);

    dispatch_input(
        &mut fixture,
        InputEvent {
            input_id: "i2".to_string(),
            text: "try again".to_string(),
            images: None,
            source: InputSource::Interactive,
            streaming_behavior: None,
        },
    )
    .await;

    let r4 = execute_tool(&fixture, "c4", json!({ "op": "status" })).await;
    assert_eq!(r4.details["stopped"], false);
}

#[tokio::test]
async fn daemon_reports_stopped_holds_for_turn() {
    let fixture = setup();

    let r1 = execute_tool(&fixture, "c1", json!({ "op": "stopped" })).await;
    assert_eq!(r1.details["ok"], true);
    assert_eq!(r1.details["stopped"], true);

    let r2 = execute_tool(&fixture, "c2", json!({ "op": "status" })).await;
    assert_eq!(r2.details["stopped"], true);
}

#[tokio::test]
async fn two_sessions_stopped_isolation() {
    let first = setup_with_session("session-1");
    let second = setup_with_session("session-2");

    let stop_res = invoke_rpc(&first, BROWSER_STOP_RPC, json!({})).await.expect("stop rpc");
    assert_eq!(stop_res["stopped"], true);

    let r1 = execute_tool(&first, "c1", json!({ "op": "status" })).await;
    assert_eq!(r1.details["stopped"], true);

    let r2 = execute_tool(&second, "c2", json!({ "op": "status" })).await;
    assert_eq!(r2.details["stopped"], false);
}

#[tokio::test]
async fn host_without_rpc_listener_is_quiet_noop() {
    let runtime = ExtensionRuntime::default();
    let events = EventBus::default();
    let mut api = ExtensionApi::new(
        LoadedExtension::new("browser-bridge", "/workspace".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        events,
        runtime.clone(),
    );
    let component = create_browser_bridge_component();
    component.register(&mut api);

    let executor = runtime
        .extension_tool_executor("browser-bridge", BROWSER_BRIDGE_TOOL_NAME)
        .expect("registered tool executor");
    let ctx = test_context("s_isolated");
    let result = executor(
        "c1",
        json!({
            "op": "state",
            "data": { "engine": "connected", "status": "idle" }
        }),
        None,
        None,
        &ctx,
    )
    .await
    .expect("execution succeeds");

    assert_eq!(result.details["ok"], true);
    assert_eq!(result.details["stopped"], false);
}

#[test]
fn tool_registration_metadata_exposure_eval_and_parallel() {
    let fixture = setup();
    let registered_tool = fixture
        .api
        .registered
        .tools
        .iter()
        .find(|t| t.definition.name == BROWSER_BRIDGE_TOOL_NAME)
        .expect("browser bridge tool found");

    assert_eq!(registered_tool.definition.exposure, Some(ToolExposure::Eval));
    assert_eq!(
        registered_tool.definition.execution_mode,
        Some(ToolExecutionMode::Parallel)
    );
    assert_eq!(registered_tool.definition.label, "Browser bridge");
    assert_eq!(
        registered_tool.definition.parameters,
        maho_omo_browser_bridge::index::parameters_schema()
    );
    let parser = registered_tool
        .definition
        .permission_parser
        .as_ref()
        .expect("permission parser registered");
    assert!(parser(&json!({ "op": "state" }), std::path::Path::new("/work")).is_empty());
}
