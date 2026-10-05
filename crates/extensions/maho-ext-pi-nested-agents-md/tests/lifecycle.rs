use maho_ext_api::*;
use maho_ext_host::ExtensionRunner;
use std::{path::{Path, PathBuf}, sync::Arc};

struct Session(PathBuf);
impl ToolSessionManager for Session {
    fn session_id(&self) -> &str { "fixture" }
    fn session_file(&self) -> Option<&Path> { Some(&self.0) }
}
impl SessionManager for Session {
    fn get_entries(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_branch(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
}
struct Registry;
struct Actions;
#[derive(Default)]
struct EntryActions(std::sync::Mutex<Vec<(String,Option<JsonValue>)>>);
impl ExtensionActions for EntryActions {
    fn send_message(&self, _: CustomMessage, _: SendMessageOptions) -> Result<(), ExtensionFailure> { Ok(()) }
    fn send_user_message(&self, _: UserMessageContent, _: SendUserMessageOptions) -> Result<(), ExtensionFailure> { Ok(()) }
    fn append_entry(&self, kind: &str, data: Option<JsonValue>) -> Result<(), ExtensionFailure> { self.0.lock().expect("capture entry").push((kind.into(),data)); Ok(()) }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> { Ok(Vec::new()) }
}
impl ExtensionActions for Actions {
    fn send_message(&self, _: CustomMessage, _: SendMessageOptions) -> Result<(), ExtensionFailure> { Ok(()) }
    fn send_user_message(&self, _: UserMessageContent, _: SendUserMessageOptions) -> Result<(), ExtensionFailure> { Ok(()) }
    fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> { Ok(()) }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> { Ok(Vec::new()) }
}
impl ModelRegistry for Registry {
    fn get_all(&self) -> Vec<Model> { Vec::new() }
    fn get_available(&self) -> Vec<Model> { Vec::new() }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None }
    fn has_configured_auth(&self, _: &Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(None) }) }
}
#[derive(Default)]
struct Ui { status: std::sync::Mutex<Option<String>>, widget: std::sync::Mutex<Option<Vec<String>>> }
impl ExtensionUi for Ui {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, value: Option<&str>) { *self.status.lock().expect("capture status") = value.map(str::to_owned); }
    fn set_widget(&self, _: &str, value: Option<WidgetContent>, _: ExtensionWidgetOptions) { *self.widget.lock().expect("capture widget") = value.and_then(|value|match value { WidgetContent::Lines(lines) => Some(lines), WidgetContent::Component(_) => None }); }
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err("headless fixture".into()) }) }
    fn theme(&self) -> Theme { Theme::default() }
}
fn fixture() -> (tempfile::TempDir, ExtensionRunner) {
    fixture_with(Vec::new())
}
fn fixture_with(mut extensions: Vec<Box<dyn Extension>>) -> (tempfile::TempDir, ExtensionRunner) {
    let tree = tempfile::tempdir().expect("create lifecycle fixture");
    std::fs::create_dir(tree.path().join("src")).expect("create source directory");
    std::fs::write(tree.path().join("src/AGENTS.md"), "fixture rules").expect("write rules");
    std::fs::write(tree.path().join("src/file.ts"), "x").expect("write source");
    let context = ExtensionContext { ui: Arc::new(Ui::default()), mode: ExtensionMode::Print, has_ui: false, cwd: tree.path().into(), agent_dir: tree.path().join("agent"),
        session_manager: Arc::new(Session(tree.path().join("a.jsonl"))), model_registry: Arc::new(Registry), model: None, thinking_level: None,
        service_tier: None, effective_service_tier: None, scoped_models: Vec::new(), goal_store_file: None,
        loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
        is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})), is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false), get_system_prompt_fn: Arc::new(String::new),
        get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default), registered_mcp_servers: Vec::new(), update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None, compaction_signal: Default::default() };
    extensions.push(Box::new(maho_ext_pi_nested_agents_md::NestedAgentsMd));
    let runner = ExtensionRunner::from_static(extensions, context);
    (tree, runner)
}
fn read_event(tree: &Path) -> ToolResultEvent {
    ToolResultEvent { tool_call_id: "read".into(), tool_name: "read".into(), input: serde_json::json!({"path":tree.join("src/file.ts")}), content: vec![ToolContent::text("block-a"), ToolContent::text("block-b")], details: Some(serde_json::json!({"preserved":true})), is_error: false, usage: None }
}
async fn content(runner: &mut ExtensionRunner, event: ToolResultEvent) -> Option<Vec<ToolContent>> {
    runner.emit_tool_result(event).await.expect("emit read result").and_then(|result| result.content)
}

#[tokio::test]
async fn content_prefix_survives_native_hook() {
    let (tree, mut runner) = fixture();
    let event = read_event(tree.path());
    let expected = event.content.clone();
    let result = content(&mut runner, event).await.expect("injected content");
    assert_eq!(&result[..2], expected);
    assert_eq!(result.len(), 3);
}

struct PriorMiddleware;
impl Extension for PriorMiddleware {
    fn register(&self, api: &mut ExtensionApi) {
        api.on(EventKind::ToolResult,Arc::new(|_,_| Box::pin(async { Ok(EventResult::ToolResult(ToolResultEventResult { content: Some(vec![ToolContent::text("modified")]), ..Default::default() })) })));
    }
}
#[tokio::test]
async fn injection_preserves_content_from_prior_native_middleware() {
    let (tree,mut runner) = fixture_with(vec![Box::new(PriorMiddleware)]);
    let result = content(&mut runner,read_event(tree.path())).await.unwrap();
    assert_eq!(result[0],ToolContent::text("modified")); assert_eq!(result.len(),2);
    assert!(serde_json::to_value(&result[1]).unwrap()["text"].as_str().unwrap().contains("fixture rules"));
}
#[tokio::test]
async fn deduplicated_until_compact() {
    let (tree, mut runner) = fixture();
    content(&mut runner, read_event(tree.path())).await.expect("first injection");
    assert!(content(&mut runner, read_event(tree.path())).await.is_none());
    runner.emit(ExtensionEvent::SessionCompact(SessionCompactEvent::Accepted { reason: CompactionReason::Manual, request_id: "fixture".into(), compaction_entry: SessionEntry { id: "compact".into(), parent_id: None, timestamp: "fixed".into(), kind: "compaction".into(), data: serde_json::json!({}) }, from_extension: false, will_retry: false })).await.expect("compact");
    assert!(content(&mut runner, read_event(tree.path())).await.is_some());
}
#[tokio::test]
async fn reinjected_after_shutdown() {
    let (tree, mut runner) = fixture();
    content(&mut runner, read_event(tree.path())).await.expect("first injection");
    runner.emit(ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Quit, target_session_file: None, signal: None })).await.expect("shutdown");
    assert!(content(&mut runner, read_event(tree.path())).await.is_some());
}
#[tokio::test]
async fn independent_session_file_has_separate_cache() {
    let (tree, mut runner) = fixture();
    content(&mut runner, read_event(tree.path())).await.expect("session a");
    let mut context = runner.create_context().expect("session context");
    context.session_manager = Arc::new(Session(tree.path().join("b.jsonl")));
    runner.bind_core(Arc::new(Actions), context);
    assert!(content(&mut runner, read_event(tree.path())).await.is_some());
}
#[tokio::test]
async fn non_read_is_unmodified() {
    let (tree, mut runner) = fixture();
    let mut event = read_event(tree.path()); event.tool_name = "bash".into();
    assert!(content(&mut runner, event).await.is_none());
}
#[tokio::test]
async fn failed_read_is_unmodified() {
    let (tree, mut runner) = fixture();
    let mut event = read_event(tree.path()); event.is_error = true;
    assert!(content(&mut runner, event).await.is_none());
}
#[tokio::test]
async fn image_only_read_is_unmodified() {
    let (tree, mut runner) = fixture();
    let mut event = read_event(tree.path()); event.content = vec![ToolContent::Image { data: "base64".into(), mime_type: "image/png".into() }];
    assert!(content(&mut runner, event).await.is_none());
}

#[tokio::test]
async fn native_toggle_reports_files_and_hides_widget() {
    let (tree,mut runner) = fixture();
    let ui = Arc::new(Ui::default());
    let mut context = runner.create_context().unwrap(); context.ui = ui.clone(); context.has_ui = true;
    runner.bind_core(Arc::new(Actions),context.clone());
    content(&mut runner,read_event(tree.path())).await.unwrap();
    assert!(ui.status.lock().unwrap().as_ref().unwrap().contains('1'));
    assert!(ui.widget.lock().unwrap().is_none());
    let command = runner.get_command("nested-agents").unwrap();
    (command.command.handler)("",&context).await.unwrap();
    assert!(ui.widget.lock().unwrap().as_ref().unwrap().iter().any(|line|line.contains("src/AGENTS.md")));
    (command.command.handler)("",&context).await.unwrap();
    assert!(ui.widget.lock().unwrap().is_none());
}

#[tokio::test]
async fn native_toggle_appends_machine_readable_cache_debug_entry() {
    let (tree,mut runner) = fixture(); let actions = Arc::new(EntryActions::default());
    let context = runner.create_context().unwrap(); runner.bind_core(actions.clone(),context.clone());
    content(&mut runner,read_event(tree.path())).await.unwrap();
    (runner.get_command("nested-agents").unwrap().command.handler)("",&context).await.unwrap();
    let entries = actions.0.lock().unwrap(); let (kind,data) = entries.last().unwrap();
    assert_eq!(kind,"nested-agents-md:debug"); let data = data.as_ref().unwrap(); assert_eq!(data["cacheSize"],1); assert_eq!(data["injectedFiles"].as_array().unwrap().len(),1);
}

#[tokio::test]
async fn headless_injection_never_updates_ui() {
    let (tree,mut runner) = fixture(); let ui = Arc::new(Ui::default());
    let mut context = runner.create_context().unwrap(); context.ui = ui.clone(); runner.bind_core(Arc::new(Actions),context);
    assert!(content(&mut runner,read_event(tree.path())).await.is_some());
    assert!(ui.status.lock().unwrap().is_none()); assert!(ui.widget.lock().unwrap().is_none());
}
#[tokio::test]
async fn disabled_flag_prevents_injection_and_widget() {
    let (tree,mut runner) = fixture(); runner.runtime.set_flag("no-nested-agents",FlagValue::Boolean(true));
    runner.emit(ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None })).await.unwrap();
    assert!(content(&mut runner,read_event(tree.path())).await.is_none());
}
#[tokio::test]
async fn truncation_metadata_reaches_native_widget() {
    let (tree,mut runner) = fixture(); std::fs::write(tree.path().join("src/AGENTS.md"),"a".repeat(200_000)).unwrap();
    let ui = Arc::new(Ui::default()); let mut context = runner.create_context().unwrap(); context.ui = ui.clone(); context.has_ui = true; runner.bind_core(Arc::new(Actions),context.clone());
    content(&mut runner,read_event(tree.path())).await.unwrap();
    (runner.get_command("nested-agents").unwrap().command.handler)("",&context).await.unwrap();
    assert!(ui.widget.lock().unwrap().as_ref().unwrap().iter().any(|line|line.contains("(truncated)")));
}
