//! Command-level cases of `help-command.test.ts` (senpi 903b8336). The overlay itself is the
//! crate's `display` seam (`native_display` mounts `HelpPanel` through `custom_factory`), so these
//! cases capture the markdown handed to it; the panel viewport case lives in tests/panel.rs.
use maho_ext_api::*;
use maho_ext_help::Help;
use std::sync::{Arc, Mutex};

struct Session;
impl ToolSessionManager for Session {
    fn session_id(&self) -> &str {
        "s"
    }
    fn session_file(&self) -> Option<&std::path::Path> {
        None
    }
}
impl SessionManager for Session {
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
struct Registry;
impl ModelRegistry for Registry {
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
#[derive(Default)]
struct Ui {
    notices: Mutex<Vec<String>>,
}
impl ExtensionUi for Ui {
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
    fn notify(&self, message: &str, _: NotificationType) {
        self.notices
            .lock()
            .expect("fixture notices")
            .push(message.into());
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
        Box::pin(async { Err("UI unavailable".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}
struct Commands;
impl ExtensionSessionActions for Commands {
    fn set_session_name(&self, _: &str) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn get_session_name(&self) -> Result<Option<String>, ExtensionFailure> {
        Err("unused".into())
    }
    fn set_label(&self, _: &str, _: Option<&str>) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn execute_tool<'a>(
        &'a self,
        name: &'a str,
        _: JsonValue,
        _: ExecuteToolOptions,
    ) -> ExecuteToolFuture<'a> {
        Box::pin(async move {
            Err(ExecuteToolError {
                code: ExecuteToolErrorCode::UnknownTool,
                tool_name: name.into(),
                message: "unused".into(),
                active_tools: vec![],
            })
        })
    }
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> {
        Ok(Vec::new())
    }
    fn set_active_tools(&self, _: Vec<String>) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn refresh_tools(&self) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn register_removed_tool_hint(&self, _: &str, _: &str) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn register_lazy_tool_activator(&self, _: LazyToolActivator) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn get_commands(&self) -> Result<Vec<SlashCommandInfo>, ExtensionFailure> {
        Ok(vec![SlashCommandInfo {
            name: "inspect-help".into(),
            description: Some("Inspect help rendering".into()),
            argument_hint: None,
            source_info: None,
        }])
    }
    fn set_model(&self, _: Model) -> ExtensionFuture<'_, bool> {
        Box::pin(async { Err("unused".into()) })
    }
    fn get_thinking_level(&self) -> Result<ThinkingLevel, ExtensionFailure> {
        Err("unused".into())
    }
    fn set_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn set_session_model(&self, _: Model) -> ExtensionFuture<'_, bool> {
        Box::pin(async { Err("unused".into()) })
    }
    fn set_session_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn set_session_fast_mode(&self, _: bool) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn exec<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: &'a std::path::Path,
        _: ExecOptions,
    ) -> ExtensionFuture<'a, ExecResult> {
        Box::pin(async { Err("unused".into()) })
    }
}
fn context(ui: Arc<Ui>, mode: ExtensionMode) -> ExtensionContext {
    ExtensionContext {
        ui,
        mode,
        has_ui: mode == ExtensionMode::Tui,
        cwd: "/repo".into(),
        agent_dir: "/agent".into(),
        session_manager: Arc::new(Session),
        model_registry: Arc::new(Registry),
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
        get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}
fn registered() -> (ExtensionApi, Arc<Mutex<Vec<String>>>) {
    let mut api = ExtensionApi::new(
        LoadedExtension::new("help", "/repo".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    let shown: Arc<Mutex<Vec<String>>> = Arc::default();
    let recorded = shown.clone();
    Help {
        display: Arc::new(move |_, markdown| {
            recorded.lock().expect("recorded help").push(markdown);
            Box::pin(async { Ok(()) })
        }),
    }
    .register(&mut api);
    api.runtime.bind_session_actions(Arc::new(Commands));
    (api, shown)
}
fn command(api: &ExtensionApi, name: &str) -> RegisteredCommand {
    api.registered
        .commands
        .iter()
        .find(|command| command.name == name)
        .cloned()
        .expect("command")
}
fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    // The handlers under test never wait on I/O; a pending poll means a handler started waiting,
    // which must fail the test instead of spinning forever.
    const MAX_POLLS: usize = 1_000;
    for _ in 0..MAX_POLLS {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
    }
    panic!("future still pending after {MAX_POLLS} polls");
}

#[test]
fn adds_help_to_the_loaded_extension_command_list() {
    let (api, _) = registered();
    let help = command(&api, "help");
    assert_eq!(
        help.description.as_deref(),
        Some("Show usage, keybindings, and all commands")
    );
    assert!(
        api.registered
            .commands
            .iter()
            .any(|command| command.name == "keybindings")
    );
}

#[test]
fn a_stop_makes_the_command_list_describe_resume_as_resuming_this_session() {
    // The same `/resume` resumes a Stop-held session and opens the picker otherwise; the list must
    // describe the behaviour the user's next Enter performs, not one fixed meaning.
    assert!(maho_ext_help::pause_state_holds_the_session(
        &serde_json::json!({
            "pauseSeq": 3,
            "holds": [{"kind": "session_hold", "key": "stop"}]
        })
    ));
    assert!(maho_ext_help::pause_state_holds_the_session(
        &serde_json::json!({
            "pauseSeq": 4,
            "holds": [{"kind": "run_fence", "key": "run:dag:r"}]
        })
    ));
    assert!(!maho_ext_help::pause_state_holds_the_session(
        &serde_json::json!({"pauseSeq": 0, "holds": []})
    ));

    let idle = maho_ext_help::help_markdown(Vec::new());
    let paused = maho_ext_help::help_markdown_for(Vec::new(), true);
    assert!(
        idle.contains("/resume — Resume this paused session, or switch to a different session")
    );
    assert!(paused.contains("/resume — Resume this paused session\n"));
}

#[test]
fn renders_all_help_sections_plus_builtin_and_extension_commands_in_a_focused_overlay() {
    let (api, shown) = registered();
    let ui = Arc::new(Ui::default());
    block_on((command(&api, "help").handler)(
        "",
        &context(ui.clone(), ExtensionMode::Tui),
    ))
    .unwrap();
    let shown = shown.lock().unwrap().clone();
    assert_eq!(shown.len(), 1);
    for section in [
        "Getting started",
        "Keybindings",
        "Commands",
        "/settings",
        "/inspect-help",
    ] {
        assert!(shown[0].contains(section), "missing {section}");
    }
    assert!(ui.notices.lock().unwrap().is_empty());
}

#[test]
fn uses_a_one_line_notification_outside_tui_mode_without_opening_custom_ui() {
    let (api, shown) = registered();
    let ui = Arc::new(Ui::default());
    block_on((command(&api, "help").handler)(
        "",
        &context(ui.clone(), ExtensionMode::Print),
    ))
    .unwrap();
    assert!(shown.lock().unwrap().is_empty());
    let notices = ui.notices.lock().unwrap().clone();
    assert_eq!(notices.len(), 1);
    assert!(!notices[0].contains('\n'));
    assert!(notices[0].contains("TUI"));
}
