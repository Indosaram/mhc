#![allow(dead_code)]
use maho_ext_api::*;
use maho_omo_skill_pointers::*;
use std::{path::Path, sync::{Arc, Mutex}};

pub struct TestSession {
    pub session_id: String,
}

impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str {
        &self.session_id
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}

impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_branch(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
}

struct TestRegistry;
impl ModelRegistry for TestRegistry {
    fn get_all(&self) -> Vec<Model> { Vec::new() }
    fn get_available(&self) -> Vec<Model> { Vec::new() }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None }
    fn has_configured_auth(&self, _: &Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}

struct TestUi;
impl ExtensionUi for TestUi {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err("UI not available".into()) }) }
    fn theme(&self) -> Theme { Theme::default() }
}

#[derive(Default)]
pub struct TestActions {
    pub messages: Mutex<Vec<CustomMessage>>,
}

impl ExtensionActions for TestActions {
    fn send_message(&self, m: CustomMessage, o: SendMessageOptions) -> Result<(), ExtensionFailure> {
        assert!(!o.trigger_turn);
        assert!(o.deliver_as.is_none());
        self.messages.lock().expect("messages lock").push(m);
        Ok(())
    }
    fn send_user_message(&self, _: UserMessageContent, _: SendUserMessageOptions) -> Result<(), ExtensionFailure> {
        panic!("unexpected send_user_message");
    }
    fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
        Ok(vec![])
    }
}

pub fn make_context(session_id: &str) -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(TestUi),
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: "/tmp".into(),
        agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(TestSession { session_id: session_id.to_string() }),
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
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions { cwd: "/tmp".into(), ..Default::default() }),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

pub struct TestHarness {
    pub api: ExtensionApi,
    pub actions: Arc<TestActions>,
}

pub fn setup_harness(skills_root: &str) -> TestHarness {
    setup_harness_with_role(skills_root, None)
}

pub fn setup_harness_with_role(skills_root: &str, role: Option<&str>) -> TestHarness {
    let actions = Arc::new(TestActions::default());
    let runtime = ExtensionRuntime::default();
    runtime.bind(actions.clone());
    let mut profile = ExtensionSessionProfile::default();
    if let Some(role) = role {
        profile.session_context.insert("role".into(), role.into());
    }
    let mut api = ExtensionApi::new(
        LoadedExtension::new("skill-pointers", "/tmp".into(), SourceInfo::default()),
        profile,
        EventBus::default(),
        runtime,
    );
    SkillPointersComponent::new(skills_root).register(&mut api);
    TestHarness { api, actions }
}

pub async fn dispatch_input(
    harness: &TestHarness,
    text: &str,
    source: InputSource,
    streaming_behavior: Option<StreamingBehavior>,
    session_id: &str,
) -> EventResult {
    let mut event = ExtensionEvent::Input(InputEvent {
        input_id: "test-input-id".into(),
        text: text.into(),
        images: None,
        source,
        streaming_behavior,
    });
    let ctx = make_context(session_id);
    let handler = harness.api.registered.handlers.get(&EventKind::Input).expect("input handler registered")[0].clone();
    handler(&mut event, &ctx).await.expect("dispatch success")
}

pub struct ExpectedPointer {
    pub custom_type: &'static str,
    pub skill_name: &'static str,
}

pub fn expect_pointer_injections(
    harness: &TestHarness,
    result: &EventResult,
    expected: &[ExpectedPointer],
) {
    match result {
        EventResult::Input(InputEventResult::Continue) => {}
        _ => panic!("expected Continue result"),
    }
    let messages = harness.actions.messages.lock().expect("messages lock");
    assert_eq!(messages.len(), expected.len());
    let actual_types: Vec<&str> = messages.iter().map(|m| m.custom_type.as_str()).collect();
    let expected_types: Vec<&str> = expected.iter().map(|e| e.custom_type).collect();
    assert_eq!(actual_types, expected_types);

    for (index, entry) in expected.iter().enumerate() {
        let msg = &messages[index];
        assert!(!msg.display);
        assert_eq!(msg.content.len(), 1);
        let text = match &msg.content[0] {
            ToolContent::Text { text, .. } => text,
            _ => panic!("expected text content"),
        };
        let tag = format!("<omo-{}-pointer>", entry.skill_name);
        assert!(text.contains(&tag), "content should contain {tag}, got {text}");
        let skill_file = format!("{}/SKILL.md", entry.skill_name);
        assert!(text.contains(&skill_file), "content should contain {skill_file}, got {text}");
    }
}

pub fn expect_no_injection(harness: &TestHarness, result: &EventResult) {
    match result {
        EventResult::Input(InputEventResult::Continue) => {}
        _ => panic!("expected Continue result"),
    }
    let messages = harness.actions.messages.lock().expect("messages lock");
    assert_eq!(messages.len(), 0);
}

pub fn injected_content(harness: &TestHarness, custom_type: &str) -> String {
    let messages = harness.actions.messages.lock().expect("messages lock");
    let msg = messages.iter().find(|m| m.custom_type == custom_type).expect("message with custom_type");
    match &msg.content[0] {
        ToolContent::Text { text, .. } => text.clone(),
        _ => panic!("expected text content"),
    }
}
