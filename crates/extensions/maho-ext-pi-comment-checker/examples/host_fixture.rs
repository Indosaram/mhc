use maho_ext_api::*;
use std::{path::Path, sync::{Arc, Mutex}};

struct Session;
impl ToolSessionManager for Session {
    fn session_id(&self) -> &str { "faux-session" }
    fn session_file(&self) -> Option<&Path> { None }
}
impl SessionManager for Session {
    fn get_entries(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_branch(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
}
struct Registry;
impl ModelRegistry for Registry {
    fn get_all(&self) -> Vec<Model> { Vec::new() }
    fn get_available(&self) -> Vec<Model> { Vec::new() }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None }
    fn has_configured_auth(&self, _: &Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(None) }) }
}
#[derive(Default)]
struct Ui { clears: Mutex<usize> }
impl ExtensionUi for Ui {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, key: &str, content: Option<WidgetContent>, options: ExtensionWidgetOptions) {
        assert_eq!(key, "pi-comment-checker");
        assert!(content.is_none());
        assert_eq!(options.placement, WidgetPlacement::AboveEditor);
        *self.clears.lock().expect("widget recorder") += 1;
    }
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err("fixture has no custom UI".into()) }) }
    fn theme(&self) -> Theme { Theme::default() }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = std::env::args().nth(1).ok_or("expected installed package source path")?;
    let fixture = tempfile::tempdir()?;
    let file = fixture.path().join("example.py");
    let text = "# Set value to 1\nvalue = 1\n";
    std::fs::write(&file, text)?;
    let ui = Arc::new(Ui::default());
    let context = context(ui.clone(), fixture.path());
    let mut runner = maho_ext_host::ExtensionRunner::from_static(vec![Box::new(maho_ext_pi_comment_checker::CommentChecker { source_path: source.into() })], context);
    runner.emit(ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None })).await?;
    let event = ToolResultEvent { tool_call_id: "write-1".into(), tool_name: "write".into(), input: serde_json::json!({"path":file,"content":text}), content: vec![ToolContent::text("write completed")], details: Some(serde_json::json!({"preserved":true})), is_error: false, usage: None };
    let result = runner.emit_tool_result(event).await?.ok_or("missing warning transformation")?;
    assert!(runner.errors.is_empty());
    let content = result.content.ok_or("missing transformed content")?;
    assert_eq!(content.len(), 2);
    assert_eq!(content[0], ToolContent::text("write completed"));
    assert_eq!(result.details, Some(serde_json::json!({"preserved":true})));
    assert_eq!(result.is_error, Some(false));
    assert_eq!(*ui.clears.lock().expect("widget recorder"), 2);
    println!("{}", serde_json::to_string(&content)?);
    drop(runner);
    Ok(())
}
fn context(ui: Arc<Ui>, cwd: &Path) -> ExtensionContext {
    ExtensionContext { ui, mode: ExtensionMode::Print, has_ui: false, cwd: cwd.into(), agent_dir: cwd.join("agent"),
        session_manager: Arc::new(Session), model_registry: Arc::new(Registry), model: None, thinking_level: None,
        service_tier: None, effective_service_tier: None, scoped_models: Vec::new(), goal_store_file: None,
        loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
        is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})), is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false), get_system_prompt_fn: Arc::new(String::new),
        get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default), registered_mcp_servers: Vec::new(), update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_pi_comment_checker::{cli::{RunResult, RunStatus}, create_comment_checker_tool_result_handler};
    async fn check(tool: &str, input: JsonValue, details: Option<JsonValue>, status: RunStatus) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&calls);
        let expected_warning = status == RunStatus::Warning;
        let handler = create_comment_checker_tool_result_handler(Arc::new(move |input| {
            captured.lock().expect("record hook").push(input);
            Box::pin(async move { RunResult { status, message: "warning".into(), binary_path: None, exit_code: None, stdout: None, stderr: None } })
        }));
        let ui = Arc::new(Ui::default());
        let ctx = context(ui.clone(), Path::new("/workspace"));
        let original = ToolContent::text("original output");
        let mut event = ExtensionEvent::ToolResult(ToolResultEvent { tool_call_id: "call".into(), tool_name: tool.into(), input, content: vec![original.clone()], details, is_error: false, usage: None });
        let result = handler(&mut event, &ctx).await.expect("run handler");
        let calls = calls.lock().expect("read hook");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].session_id, "faux-session");
        assert_eq!(calls[0].cwd, "/workspace");
        assert_eq!(calls[0].hook_event_name, "PostToolUse");
        if expected_warning {
            let EventResult::ToolResult(result) = result else { panic!("expected warning content"); };
            assert_eq!(result.content, Some(vec![original, ToolContent::text("\n\nwarning")]));
        } else { assert!(matches!(result, EventResult::None)); }
        assert_eq!(*ui.clears.lock().expect("widget recorder"), 1);
    }
    #[tokio::test] async fn patch_warning() { check("apply_patch", serde_json::json!({}), Some(serde_json::json!({"files":[{"filePath":"a.ts","before":"before","after":"after","type":"update"}]})), RunStatus::Warning).await; }
    #[tokio::test] async fn missing_hidden() { check("write", serde_json::json!({"path":"a.ts","content":"x"}), None, RunStatus::Missing).await; }
    #[tokio::test] async fn write_warning() { check("write", serde_json::json!({"path":"a.ts","content":"x"}), None, RunStatus::Warning).await; }
    #[tokio::test] async fn write_clean() { check("write", serde_json::json!({"path":"a.ts","content":"x"}), None, RunStatus::Pass).await; }
    #[tokio::test] async fn edit_warning() { check("edit", serde_json::json!({"filePath":"a.ts","oldString":"before","newString":"after"}), None, RunStatus::Warning).await; }
    #[tokio::test] async fn checker_error() { check("write", serde_json::json!({"path":"a.ts","content":"x"}), None, RunStatus::Error).await; }
}
