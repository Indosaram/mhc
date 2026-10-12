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
fn read_event(path: &Path, is_error: bool) -> ExtensionEvent {
    ExtensionEvent::ToolResult(ToolResultEvent { tool_name: "read".into(), tool_call_id: "read-1".into(), input: serde_json::json!({"path":path}), content: vec![ToolContent::text("target")], details: None, is_error, usage: None })
}
fn result_value(result: EventResult) -> serde_json::Value {
    match result { EventResult::None => serde_json::Value::Null, EventResult::ToolResult(result) => serde_json::json!({"content":result.content.expect("tool result content")}), _ => panic!("unexpected tool result") }
}
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

#[tokio::test]
async fn lifecycle_events_reset_and_preserve_injection_cache() {
 let root = tempfile::tempdir().unwrap(); let child = root.path().join("nested"); std::fs::create_dir(&child).unwrap();
 std::fs::write(child.join("AGENTS.md"), "fixture rule").unwrap(); let target = child.join("file.rs"); std::fs::write(&target, "target").unwrap();
 let mut ctx = context(); ctx.cwd = root.path().into(); let mut api = api(); maho_ext_nested_agents_md::NestedAgentsMd.register(&mut api);
 let mut start = ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None });
 api.registered.handlers[&EventKind::SessionStart][0](&mut start, &ctx).await.unwrap();
 let mut results = Vec::new();
 for step in 0..6 {
  let transition = match step {
   2 => Some(ExtensionEvent::SessionCompact(SessionCompactEvent::Rejected { reason: CompactionReason::Extension, request_id: "compact".into(), rejection_cause: CompactionRejectionCause::CancelledByExtension })),
   3 => Some(ExtensionEvent::SessionCompact(SessionCompactEvent::Accepted { reason: CompactionReason::Extension, request_id: "compact".into(), compaction_entry: SessionEntry { id: "compact".into(), parent_id: None, timestamp: "0".into(), kind: "compaction".into(), data: serde_json::Value::Null }, from_extension: true, will_retry: false })),
   4 => Some(ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Quit, target_session_file: None, signal: None })),
   _ => None,
  };
  if let Some(mut event) = transition { api.registered.handlers[&event.kind()][0](&mut event, &ctx).await.unwrap(); }
  let mut event = read_event(&target, step == 5);
  let result = api.registered.handlers[&EventKind::ToolResult][0](&mut event, &ctx).await.unwrap();
  results.push(result_value(result));
 }
 let results = serde_json::to_string(&results).unwrap().replace(root.path().to_str().unwrap(), "<root>");
 let results: serde_json::Value = serde_json::from_str(&results).unwrap();
 assert_eq!(results[0]["content"][1]["audience"], "model");
 assert_eq!(results[1], serde_json::Value::Null);
 assert_eq!(results[2], serde_json::Value::Null);
 assert_eq!(results[3]["content"][1]["audience"], "model");
 assert_eq!(results[4]["content"][1]["audience"], "model");
 assert_eq!(results[5], serde_json::Value::Null);
}

#[tokio::test]
async fn toggle_command_emits_widget_and_machine_debug_fields() {
 struct Actions(Arc<std::sync::Mutex<Vec<serde_json::Value>>>);
 impl ExtensionActions for Actions {
  fn send_message(&self, _: CustomMessage, _: SendMessageOptions) -> Result<(), ExtensionFailure> { panic!("unexpected message") }
  fn send_user_message(&self, _: UserMessageContent, _: SendUserMessageOptions) -> Result<(), ExtensionFailure> { panic!("unexpected user message") }
  fn append_entry(&self, kind: &str, data: Option<JsonValue>) -> Result<(), ExtensionFailure> { self.0.lock().unwrap().push(serde_json::json!({"type":kind,"data":data})); Ok(()) }
  fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> { Ok(vec![]) }
 }
 let root = tempfile::tempdir().unwrap(); let child = root.path().join("nested"); std::fs::create_dir(&child).unwrap();
 let instruction = child.join("AGENTS.md"); std::fs::write(&instruction, "fixture rule").unwrap(); let target = child.join("file.rs"); std::fs::write(&target, "target").unwrap();
 let captured = Arc::new(std::sync::Mutex::new(Vec::new())); let mut ctx = context(); ctx.cwd = root.path().into();
 ctx.ui = Arc::new(CapturingUi { capture: Arc::clone(&captured) });
 let mut api = api(); api.runtime.bind(Arc::new(Actions(Arc::clone(&captured)))); maho_ext_nested_agents_md::NestedAgentsMd.register(&mut api);
 let mut event = read_event(&target, false); api.registered.handlers[&EventKind::ToolResult][0](&mut event, &ctx).await.unwrap(); ctx.has_ui = true;
 let command = &api.registered.commands.iter().find(|command| command.name == "nested-agents").unwrap().handler;
 command("", &ctx).await.unwrap(); command("", &ctx).await.unwrap();
 let output = captured.lock().unwrap().clone();
 assert_eq!(output[0], serde_json::json!({"widget":["Nested Context:", "  nested/AGENTS.md"]}));
 assert_eq!(output[1]["type"], "nested-agents-md:debug");
 let data = &output[1]["data"];
 assert_eq!(data["sessionKey"], "__pi_nested_agents_md_singleton__");
 assert_eq!(data["cacheSize"], 1);
 assert_eq!(data["injectedDirectories"][0], root.path().join("nested").to_string_lossy().as_ref());
 assert_eq!(data["injectedFiles"][0]["path"], instruction.to_string_lossy().as_ref());
 assert_eq!(data["injectedFiles"][0]["truncated"], false);
 assert_eq!(output[3], serde_json::json!({"widget":null}));
 assert_eq!(output[4]["data"]["cacheSize"], 1);
}

#[derive(Default)]
struct CapturingUi { capture: Arc<std::sync::Mutex<Vec<serde_json::Value>>> }
impl ExtensionUi for CapturingUi {
 fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
 fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
 fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
 fn notify(&self, message: &str, level: NotificationType) { self.capture.lock().expect("ui capture").push(serde_json::json!({"message":message,"level":match level { NotificationType::Info => "info", NotificationType::Warning => "warning", NotificationType::Error => "error" }})); }
 fn set_status(&self, _: &str, _: Option<&str>) {}
 fn set_widget(&self, _: &str, content: Option<WidgetContent>, _: ExtensionWidgetOptions) { let value = match content { Some(WidgetContent::Lines(lines)) => serde_json::json!(lines), None => serde_json::Value::Null, _ => panic!("unexpected widget factory") }; self.capture.lock().expect("ui capture").push(serde_json::json!({"widget":value})); }
 fn set_header(&self, _: Option<ComponentFactory>) {}
 fn set_footer(&self, _: Option<ComponentFactory>) {}
 fn set_title(&self, _: &str) {}
 fn paste_to_editor(&self, _: &str) {}
 fn set_editor_text(&self, _: &str) {}
 fn get_editor_text(&self) -> String { String::new() }
 fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err("UI not available".into()) }) }
 fn theme(&self) -> Theme { Theme::default() }
}

#[tokio::test]
async fn instruction_read_failure_does_not_poison_recovery() {
 let root = tempfile::tempdir().unwrap(); let child = root.path().join("nested"); std::fs::create_dir(&child).unwrap();
 let instruction = child.join("AGENTS.md"); std::fs::create_dir(&instruction).unwrap(); let target = child.join("file.rs"); std::fs::write(&target, "target").unwrap();
 let mut ctx = context(); ctx.cwd = root.path().into(); let mut api = api(); maho_ext_nested_agents_md::NestedAgentsMd.register(&mut api);
 let mut failed = read_event(&target, false); let first = api.registered.handlers[&EventKind::ToolResult][0](&mut failed, &ctx).await.unwrap();
 assert!(matches!(first, EventResult::None));
 std::fs::remove_dir(&instruction).unwrap(); std::fs::write(&instruction, "fixture rule").unwrap();
 let mut recovered = read_event(&target, false); let second = api.registered.handlers[&EventKind::ToolResult][0](&mut recovered, &ctx).await.unwrap();
 match second { EventResult::ToolResult(result) => assert!(result.content.unwrap().iter().any(|item| matches!(item, ToolContent::Text { text, audience: Some(audience) } if audience == "model" && text.ends_with("fixture rule")))), _ => panic!("recovered instruction read did not inject context") }
}
