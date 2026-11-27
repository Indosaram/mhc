//! Port of `packages/coding-agent/test/suite/{kill-guard-bash-input,kill-guard-extension}.test.ts`.

use maho_core::kill_guard::{KILL_GUARD_TAG, active_kill_guard_environment};
use maho_ext_api::{
    EventBus, EventKind, EventResult, Extension, ExtensionApi, ExtensionContext, ExtensionEvent,
    ExtensionFailure, ExtensionMode, ExtensionRuntime, ExtensionSessionProfile, ExtensionUi,
    ExtensionUiDialogOptions, JsonValue, LoadedExtension, Model, ModelRegistry, NotificationType,
    SessionEntry, SessionManager, SessionReason, SessionShutdownEvent, SessionStartEvent,
    SourceInfo, Theme, ToolCallEvent, ToolCallEventResult, ToolSessionManager, UiFuture,
};
use maho_ext_kill_guard::{KillGuardExtension, command_of, plain_submitted_lines};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

const REPORTER: &str = r#"pkill -f 'paid" 2>/dev/null || echo 1' ; true"#;

struct FakeSession(String);

impl ToolSessionManager for FakeSession {
    fn session_id(&self) -> &str {
        &self.0
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}

impl SessionManager for FakeSession {
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

struct FakeModels;

impl ModelRegistry for FakeModels {
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
    fn get_api_key_for_provider<'a>(
        &'a self,
        _: &'a str,
    ) -> maho_ext_api::ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}

struct RecordingUi {
    notices: Mutex<Vec<(String, NotificationType)>>,
}

impl RecordingUi {
    fn new() -> Self {
        Self {
            notices: Mutex::new(Vec::new()),
        }
    }

    fn notices(&self) -> Vec<String> {
        self.notices
            .lock()
            .expect("notices")
            .iter()
            .map(|(msg, _)| msg.clone())
            .collect()
    }
}

impl ExtensionUi for RecordingUi {
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
    fn notify(&self, message: &str, kind: NotificationType) {
        self.notices
            .lock()
            .expect("notices")
            .push((message.to_owned(), kind));
    }
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(
        &self,
        _: &str,
        _: Option<maho_ext_api::WidgetContent>,
        _: maho_ext_api::ExtensionWidgetOptions,
    ) {
    }
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(
        &self,
        _: maho_ext_api::ComponentFactory,
        _: maho_ext_api::CustomUiOptions,
    ) -> maho_ext_api::ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Ok(JsonValue::Null) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

fn make_ctx(cwd: &Path, agent_dir: &Path, ui: Arc<RecordingUi>) -> ExtensionContext {
    ExtensionContext {
        ui,
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: cwd.to_path_buf(),
        agent_dir: agent_dir.to_path_buf(),
        session_manager: Arc::new(FakeSession("kg-test".into())),
        model_registry: Arc::new(FakeModels),
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

struct ExtHarness {
    _temp: tempfile::TempDir,
    api: ExtensionApi,
    ctx: ExtensionContext,
    ui: Arc<RecordingUi>,
}

impl ExtHarness {
    async fn start(user_settings: Option<&str>, project_settings: Option<&str>) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let cwd = temp.path().join("cwd");
        let agent_dir = temp.path().join("agent");
        std::fs::create_dir_all(&cwd).expect("mkdir cwd");
        std::fs::create_dir_all(&agent_dir).expect("mkdir agent");
        if let Some(json_str) = user_settings {
            std::fs::write(agent_dir.join("settings.json"), json_str).expect("write user settings");
        }
        if let Some(json_str) = project_settings {
            std::fs::create_dir_all(cwd.join(".senpi")).expect("mkdir .senpi");
            std::fs::write(cwd.join(".senpi").join("settings.json"), json_str)
                .expect("write project settings");
        }

        let mut api = ExtensionApi::new(
            LoadedExtension::new("kill-guard", cwd.clone(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        );
        KillGuardExtension.register(&mut api);
        let ui = Arc::new(RecordingUi::new());
        let ctx = make_ctx(&cwd, &agent_dir, Arc::clone(&ui));
        for handler in &api.registered.handlers[&EventKind::SessionStart] {
            let mut event = ExtensionEvent::SessionStart(SessionStartEvent {
                sizes_only: false,
                reason: SessionReason::Startup,
                initial_model_provenance: None,
                previous_session_file: None,
                attended: Some(false),
            });
            handler(&mut event, &ctx).await.expect("session start");
        }
        Self {
            _temp: temp,
            api,
            ctx,
            ui,
        }
    }

    async fn call(
        &self,
        tool_name: &str,
        input: JsonValue,
    ) -> Result<Option<String>, ExtensionFailure> {
        for handler in &self.api.registered.handlers[&EventKind::ToolCall] {
            let mut event = ExtensionEvent::ToolCall(ToolCallEvent {
                tool_call_id: "call-1".into(),
                tool_name: tool_name.into(),
                input: input.clone(),
                parent_tool_call_id: None,
            });
            if let EventResult::ToolCall(ToolCallEventResult {
                block: Some(true),
                reason,
                ..
            }) = handler(&mut event, &self.ctx).await?
            {
                return Ok(Some(reason.unwrap_or_default()));
            }
        }
        Ok(None)
    }

    async fn shutdown(&self) {
        for handler in &self.api.registered.handlers[&EventKind::SessionShutdown] {
            let mut event = ExtensionEvent::SessionShutdown(SessionShutdownEvent {
                reason: SessionReason::Quit,
                target_session_file: None,
                signal: None,
            });
            handler(&mut event, &self.ctx)
                .await
                .expect("session shutdown");
        }
    }
}

#[test]
fn plain_submitted_lines_and_command_of_match_ts() {
    assert_eq!(command_of("bash", &json!({"command": "ls"})), Some("ls"));
    assert_eq!(
        command_of("monitor", &json!({"command": "tail -f x"})),
        Some("tail -f x")
    );
    assert_eq!(command_of("read", &json!({"command": "ls"})), None);

    // Submitted cases
    assert_eq!(
        plain_submitted_lines(&json!({"input": "kill -9 -1"})),
        Some(vec!["kill -9 -1".to_owned()])
    );
    assert_eq!(
        plain_submitted_lines(&json!({"input": "kill -9 -1\n", "submit": false})),
        Some(vec!["kill -9 -1".to_owned()])
    );
    assert_eq!(
        plain_submitted_lines(&json!({"input": "kill -9 -1", "submit": false, "keys": ["enter"]})),
        Some(vec!["kill -9 -1".to_owned()])
    );
    assert_eq!(
        plain_submitted_lines(&json!({"input": "kilk\x7fl -9 -2\x081"})),
        Some(vec!["kill -9 -1".to_owned()])
    );
    assert_eq!(
        plain_submitted_lines(&json!({"input": "echo ok\nkill -9 -1\npartial", "submit": false})),
        Some(vec!["echo ok".to_owned(), "kill -9 -1".to_owned()])
    );

    // Non-plain / unsubmitted cases
    assert_eq!(
        plain_submitted_lines(&json!({"input": "kill -9 -1", "submit": false})),
        Some(Vec::new())
    );
    assert_eq!(
        plain_submitted_lines(&json!({"input": "ill -9 -1\x01k\n", "submit": false})),
        None
    );
    assert_eq!(
        plain_submitted_lines(&json!({"input": "ill -9 -1\x1b[Hk"})),
        None
    );
    assert_eq!(
        plain_submitted_lines(&json!({"input": "kil\t -9 -1\n", "submit": false})),
        None
    );
    assert_eq!(
        plain_submitted_lines(&json!({"input": "\x1b:wq\r", "submit": false})),
        None
    );
    assert_eq!(plain_submitted_lines(&json!({"keys": ["escape"]})), None);
    assert_eq!(plain_submitted_lines(&json!({"keys": ["ctrl+z"]})), None);
    assert_eq!(
        plain_submitted_lines(&json!({"keys": ["up", "enter"]})),
        None
    );
    assert_eq!(
        plain_submitted_lines(
            &json!({"input": "kill -9 -1", "submit": false, "keys": ["ctrl+u", "enter"]})
        ),
        None
    );
}

#[tokio::test(flavor = "current_thread")]
async fn kill_guard_extension_blocks_shell_tools_respects_user_off_and_ignores_project_off() {
    let h = ExtHarness::start(None, None).await;

    for (tool, params) in [
        ("bash", json!({ "command": REPORTER })),
        (
            "monitor",
            json!({ "command": REPORTER, "description": "watch" }),
        ),
        ("bash_input", json!({ "bash_id": "b1", "input": REPORTER })),
        (
            "bash_input",
            json!({ "bash_id": "b1", "input": REPORTER, "keys": ["enter"], "submit": false }),
        ),
        (
            "bash_input",
            json!({ "bash_id": "b1", "input": "kill -9 -1\n", "submit": false }),
        ),
        (
            "bash_input",
            json!({ "bash_id": "b1", "input": "kill -9 -1\r", "submit": false }),
        ),
        (
            "bash_input",
            json!({ "bash_id": "b1", "input": "echo ok\nkill -9 -1\npartial", "submit": false }),
        ),
        ("bash", json!({ "command": "kill -9 -1" })),
    ] {
        let blocked = h.call(tool, params.clone()).await.expect("call ok");
        let reason = blocked.unwrap_or_else(|| panic!("expected refusal for {tool} {params}"));
        assert!(reason.contains(KILL_GUARD_TAG));
    }

    // Unsubmitted bash_input & narrow commands are allowed
    assert_eq!(
        h.call(
            "bash_input",
            json!({ "bash_id": "b1", "input": REPORTER, "submit": false }),
        )
        .await
        .expect("call ok"),
        None
    );
    for cmd in [
        "echo kill -9 -1",
        "pgrep -af node",
        "kill -0 12345",
        "docker kill web",
    ] {
        assert_eq!(
            h.call("bash", json!({ "command": cmd }))
                .await
                .expect("call ok"),
            None
        );
    }

    // Switching off in another session leaves the first session's runtime layer active
    let agent_shim = h.ctx.agent_dir.join("kill-guard");
    let off = ExtHarness::start(Some(r#"{"terminal":{"killGuard":"off"}}"#), None).await;
    assert_eq!(
        off.call("bash", json!({ "command": REPORTER }))
            .await
            .expect("call ok"),
        None
    );
    assert_eq!(
        off.ui
            .notices()
            .iter()
            .filter(|n| n.contains("kill guard"))
            .count(),
        1
    );
    #[cfg(unix)]
    {
        let env = active_kill_guard_environment(&BTreeMap::from([(
            "PATH".into(),
            "/usr/bin:/bin".into(),
        )]));
        assert!(
            env.get("PATH")
                .is_some_and(|p| p.contains(agent_shim.to_string_lossy().as_ref())),
            "session A's runtime layer must stay active while session B disabled"
        );
    }
    off.shutdown().await;

    // Project setting attempting to switch off is ignored
    let project_off = ExtHarness::start(None, Some(r#"{"terminal":{"killGuard":"off"}}"#)).await;
    let blocked = project_off
        .call("bash", json!({ "command": REPORTER }))
        .await
        .expect("call ok");
    assert!(blocked.is_some_and(|r| r.contains(KILL_GUARD_TAG)));

    project_off.shutdown().await;
    h.shutdown().await;
}
