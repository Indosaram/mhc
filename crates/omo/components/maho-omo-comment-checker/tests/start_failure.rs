#![cfg(unix)]

use std::fs;
use std::sync::{Arc, Mutex};

use comment_checker_core::{CheckFailure, CheckResult, HookInput, HookToolInput, RunCommentCheckerInput};
use maho_ext_api::*;
use maho_omo_comment_checker::component::CommentCheckerComponent;
use maho_omo_comment_checker::failure_notice::COMMENT_CHECKER_COULD_NOT_RUN;
use maho_omo_comment_checker::runner::default_run_comment_checker;
use maho_omo_comment_checker::types::CommentCheckerComponentOptions;
use tempfile::tempdir;

#[derive(Default)]
struct TestLogger {
    entries: Mutex<Vec<(String, String, Option<serde_json::Value>)>>,
}

impl ComponentLogger for TestLogger {
    fn info(&self, message: &str, details: Option<&serde_json::Value>) {
        self.entries
            .lock()
            .expect("test assertion")
            .push(("info".to_string(), message.to_string(), details.cloned()));
    }
    fn warn(&self, message: &str, details: Option<&serde_json::Value>) {
        self.entries
            .lock()
            .expect("test assertion")
            .push(("warn".to_string(), message.to_string(), details.cloned()));
    }
    fn error(&self, message: &str, details: Option<&serde_json::Value>) {
        self.entries
            .lock()
            .expect("test assertion")
            .push(("error".to_string(), message.to_string(), details.cloned()));
    }
}

#[derive(Default)]
struct TestUi {
    notices: Mutex<Vec<(String, NotificationType)>>,
}

impl ExtensionUi for TestUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> {
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
    fn notify(&self, message: &str, level: NotificationType) {
        self.notices
            .lock()
            .expect("test assertion")
            .push((message.to_string(), level));
    }
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

struct TestSession;
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str {
        "s"
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

fn test_context(
    logger: Arc<TestLogger>,
    ui: Arc<TestUi>,
    cwd: std::path::PathBuf,
) -> ExtensionContext {
    ExtensionContext {
        ui,
        mode: ExtensionMode::Print,
        has_ui: true,
        cwd: cwd.clone(),
        agent_dir: cwd,
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
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: Some(logger),
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

async fn two_edits(
    result: CheckResult,
) -> (
    usize,
    Arc<TestLogger>,
    Arc<TestUi>,
    EventResult,
    EventResult,
) {
    let calls = Arc::new(Mutex::new(0));
    let calls_clone = calls.clone();

    let mut api = ExtensionApi::new(
        LoadedExtension::new("checker", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );

    let res = result.clone();
    CommentCheckerComponent {
        options: CommentCheckerComponentOptions {
            resolve_binary: Some(Arc::new(|| Some("/tmp/fake-comment-checker".into()))),
            run_comment_checker: Some(Arc::new(move |_| {
                *calls_clone.lock().expect("test assertion") += 1;
                let r = res.clone();
                Box::pin(async move { Ok(r) })
            })),
            download_binary: None,
        },
        package_binary: None,
    }
    .register(&mut api);

    let logger = Arc::new(TestLogger::default());
    let ui = Arc::new(TestUi::default());
    let temp = tempdir().expect("test assertion");
    let ctx = test_context(logger.clone(), ui.clone(), temp.path().to_path_buf());

    let mut event1 = ExtensionEvent::ToolResult(ToolResultEvent {
        tool_name: "write".into(),
        tool_call_id: "tool-1".into(),
        input: serde_json::json!({"path": "src/a.ts", "content": "x"}),
        content: vec![],
        details: None,
        is_error: false,
        usage: None,
    });
    let first = api.registered.handlers[&EventKind::ToolResult][0](&mut event1, &ctx)
        .await
        .expect("test assertion");

    let mut event2 = ExtensionEvent::ToolResult(ToolResultEvent {
        tool_name: "edit".into(),
        tool_call_id: "tool-2".into(),
        input: serde_json::json!({"path": "src/other.ts", "edits": []}),
        content: vec![],
        details: None,
        is_error: false,
        usage: None,
    });
    let second = api.registered.handlers[&EventKind::ToolResult][0](&mut event2, &ctx)
        .await
        .expect("test assertion");

    let num_calls = *calls.lock().expect("test assertion");
    (num_calls, logger, ui, first, second)
}

#[tokio::test]
async fn given_checker_exiting_outside_protocol_when_two_edits_finish_then_reported_once_and_inert()
{
    let (calls, logger, ui, first, second) = two_edits(CheckResult {
        has_comments: false,
        message: String::new(),
        failure: Some(CheckFailure {
            exit_code: Some(3221225781_i64 as i32),
            stderr: String::new(),
        }),
    })
    .await;

    assert_eq!(calls, 1);
    assert!(matches!(first, EventResult::None));
    assert!(matches!(second, EventResult::None));

    let logs = logger.entries.lock().expect("test assertion").clone();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].0, "warn");
    assert_eq!(logs[0].1, COMMENT_CHECKER_COULD_NOT_RUN);

    let notices = ui.notices.lock().expect("test assertion").clone();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].1, NotificationType::Warning);
    assert!(notices[0].0.contains(&format!("{}", 3221225781_i64 as i32)));
    assert!(notices[0].0.contains("/tmp/fake-comment-checker"));
}

#[tokio::test]
async fn given_checker_that_never_started_when_two_edits_finish_then_reported_once_and_inert() {
    let (calls, logger, ui, _first, _second) = two_edits(CheckResult {
        has_comments: false,
        message: String::new(),
        failure: Some(CheckFailure {
            exit_code: None,
            stderr: "spawn /tmp/fake-comment-checker EACCES".to_string(),
        }),
    })
    .await;

    assert_eq!(calls, 1);
    let logs = logger.entries.lock().expect("test assertion").clone();
    assert_eq!(logs.len(), 1);
    let notices = ui.notices.lock().expect("test assertion").clone();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].0.contains("EACCES"));
}

#[tokio::test]
async fn given_checker_following_protocol_when_two_edits_finish_then_keeps_running_without_warnings()
{
    let (calls, logger, ui, _first, _second) = two_edits(CheckResult {
        has_comments: false,
        message: String::new(),
        failure: None,
    })
    .await;

    assert_eq!(calls, 2);
    assert!(logger.entries.lock().expect("test assertion").is_empty());
    assert!(ui.notices.lock().expect("test assertion").is_empty());
}

#[tokio::test]
#[cfg(unix)]
async fn given_checker_without_execute_permission_when_real_runner_spawns_it_then_start_failure_exposed()
{
    use std::os::unix::fs::PermissionsExt;
    let temp = tempdir().expect("test assertion");
    let binary_path = temp.path().join("comment-checker");
    fs::write(&binary_path, "#!/bin/sh
exit 0
").expect("test assertion");
    fs::set_permissions(&binary_path, fs::Permissions::from_mode(0o644)).expect("test assertion");

    let result = default_run_comment_checker(&RunCommentCheckerInput {
        binary_path: Some(binary_path.to_string_lossy().into_owned()),
        hook_input: HookInput {
            session_id: "s".into(),
            tool_name: "write".into(),
            transcript_path: "".into(),
            cwd: ".".into(),
            hook_event_name: "PostToolUse".into(),
            tool_input: HookToolInput {
                file_path: Some("src/a.ts".into()),
                content: Some("x".into()),
                old_string: None,
                new_string: None,
                edits: None,
            },
            tool_response: None,
        },
        custom_prompt: None,
    })
    .await
    .expect("test assertion");

    assert!(!result.has_comments);
    let failure = result.failure.expect("failure exposed");
    assert_eq!(failure.exit_code, None);
    assert!(failure.stderr.contains("Permission denied") || failure.stderr.contains("EACCES"));
}
