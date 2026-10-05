use maho_ext_api::*;
use std::{path::Path, sync::Arc};

struct TestSession;
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str { "session" }
    fn session_file(&self) -> Option<&Path> { None }
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
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(None) }) }
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
fn context() -> ExtensionContext {
    ExtensionContext { ui: Arc::new(TestUi), mode: ExtensionMode::Print, has_ui: false, cwd: "/tmp".into(), agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(TestSession), model_registry: Arc::new(TestRegistry), model: None, thinking_level: None,
        service_tier: None, effective_service_tier: None, scoped_models: Vec::new(), goal_store_file: None,
        loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
        is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})), is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false), get_system_prompt_fn: Arc::new(|| "base".into()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions { cwd: "/tmp".into(), ..Default::default() }),
        registered_mcp_servers: Vec::new(), update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None, compaction_signal: Default::default() }
}

fn api() -> ExtensionApi { ExtensionApi::new(LoadedExtension::new("nested", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default()) }
#[test]
fn extension_registers_nested_command_and_lifecycle_hooks() {
 let mut api = api();
 maho_ext_nested_agents_md::NestedAgentsMd.register(&mut api);
 assert!(api.registered.commands.iter().any(|command| command.name == "nested-agents"));
 assert_eq!(api.registered.handlers.len(), 4);
}
#[tokio::test]
async fn read_hook_appends_model_only_directory_context() {
 let root = tempfile::tempdir().unwrap(); let child = root.path().join("nested"); std::fs::create_dir(&child).unwrap(); let target = child.join("file.rs"); std::fs::write(&target, "target").unwrap(); let instructions = child.join("AGENTS.md"); std::fs::write(&instructions, "fixture rule").unwrap(); let mut ctx = context(); ctx.cwd = root.path().into(); let mut api = api(); maho_ext_nested_agents_md::NestedAgentsMd.register(&mut api); let mut event = ExtensionEvent::ToolResult(ToolResultEvent { tool_name: "read".into(), tool_call_id: "read-1".into(), input: serde_json::json!({"path":target}), content: vec![ToolContent::text("target")], details: None, is_error: false, usage: None });
 let result = api.registered.handlers[&EventKind::ToolResult][0](&mut event, &ctx).await.unwrap();
 match result { EventResult::ToolResult(result) => assert_eq!(result.content.unwrap(), vec![ToolContent::text("target"), ToolContent::Text { text: format!("\n\n[Directory Context: {}]\nfixture rule", instructions.display()), audience: Some("model".into()) }]), _ => panic!("missing tool result") }
}
#[tokio::test]
async fn failed_read_does_not_inject_context() {
 let ctx = context(); let mut api = api(); maho_ext_nested_agents_md::NestedAgentsMd.register(&mut api); let mut event = ExtensionEvent::ToolResult(ToolResultEvent { tool_name: "read".into(), tool_call_id: "read-1".into(), input: serde_json::json!({"path":"missing"}), content: vec![ToolContent::text("error")], details: None, is_error: true, usage: None });
 let result = api.registered.handlers[&EventKind::ToolResult][0](&mut event, &ctx).await.unwrap();
 assert!(matches!(result, EventResult::None));
}
