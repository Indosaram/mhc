use maho_test_support::{
    faux::{FauxResponse, FauxScript},
    faux_session::FauxSession,
};

/// The mounted host's history-fetch channel sender, shared behind the mode's mount.
type HistorySender = std::sync::Arc<
    std::sync::Mutex<
        Option<tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>>,
    >,
>;

struct EditorHost;
impl maho_tui::components::editor::EditorTuiHost for EditorHost {
    fn request_render(&self) {}
    fn terminal_rows(&self) -> usize {
        36
    }
}

fn native_mode() -> (
    maho_interactive::interactive_mode::InteractiveMode,
    tempfile::TempDir,
) {
    native_mode_with_provider(None, "faux", "faux-1", false)
}

fn native_mode_with_provider(
    cwd_override: Option<&str>,
    provider_id: &str,
    model_id: &str,
    unknown_initial_model: bool,
) -> (
    maho_interactive::interactive_mode::InteractiveMode,
    tempfile::TempDir,
) {
    native_mode_with_session_and_provider(
        cwd_override,
        |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
        provider_id,
        model_id,
        unknown_initial_model,
    )
}

fn native_mode_at(
    cwd_override: Option<&str>,
) -> (
    maho_interactive::interactive_mode::InteractiveMode,
    tempfile::TempDir,
) {
    native_mode_with_provider(cwd_override, "faux", "faux-1", false)
}

fn native_mode_with_session(
    cwd_override: Option<&str>,
    session_manager: impl FnOnce(&str) -> maho_core::session_manager::SessionManager,
) -> (
    maho_interactive::interactive_mode::InteractiveMode,
    tempfile::TempDir,
) {
    native_mode_with_session_and_provider(cwd_override, session_manager, "faux", "faux-1", false)
}

fn native_mode_with_session_and_provider(
    cwd_override: Option<&str>,
    session_manager: impl FnOnce(&str) -> maho_core::session_manager::SessionManager,
    provider_id: &str,
    model_id: &str,
    unknown_initial_model: bool,
) -> (
    maho_interactive::interactive_mode::InteractiveMode,
    tempfile::TempDir,
) {
    let (mode, directory, _) = native_mode_with_session_and_provider_retaining_session(
        cwd_override,
        session_manager,
        provider_id,
        model_id,
        unknown_initial_model,
    );
    (mode, directory)
}

fn native_mode_with_session_and_provider_retaining_session(
    cwd_override: Option<&str>,
    session_manager: impl FnOnce(&str) -> maho_core::session_manager::SessionManager,
    provider_id: &str,
    model_id: &str,
    unknown_initial_model: bool,
) -> (
    maho_interactive::interactive_mode::InteractiveMode,
    tempfile::TempDir,
    std::sync::Arc<maho_core::agent_session::AgentSession>,
) {
    use maho_ai::providers::faux::{
        FauxAssistantMessageOptions, FauxModelDefinition, RegisterFauxProviderOptions,
        faux_assistant_message, faux_provider, faux_streams,
    };
    use maho_core::agent_session::{AgentSession, AgentSessionConfig};
    use std::sync::Arc;
    let directory = tempfile::tempdir().expect("directory");
    let cwd = cwd_override.map_or_else(
        || directory.path().to_string_lossy().into_owned(),
        str::to_owned,
    );
    let provider = faux_provider(RegisterFauxProviderOptions {
        api: Some("faux".into()),
        provider: Some(provider_id.into()),
        models: Some(vec![FauxModelDefinition {
            id: model_id.into(),
            ..Default::default()
        }]),
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("valid faux provider");
    let model = if unknown_initial_model {
        maho_ai::types::Model {
            provider: "unknown".into(),
            id: "unknown".into(),
            ..provider.get_model(Some(model_id)).expect("model")
        }
    } else {
        provider.get_model(Some(model_id)).expect("model")
    };
    provider.set_responses(vec![
        faux_assistant_message(
            "hello",
            FauxAssistantMessageOptions {
                timestamp: Some(0),
                ..Default::default()
            },
        )
        .into(),
    ]);
    let streams = faux_streams(provider.core.clone());
    let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| {
        let mut context = context.clone();
        context.system_prompt = context.system_prompt.filter(|prompt| !prompt.is_empty());
        streams.stream_simple(model, &context, options.map(|options| options.simple))
    });
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(
        maho_core::model_runtime::CreateModelRuntimeOptions {
            models_path: Some(directory.path().join("models.json")),
            auth_path: Some(directory.path().join("auth.json")),
            providers: Some(vec![provider.provider.clone()]),
            ..Default::default()
        },
    )
    .expect("valid model runtime");
    let session = AgentSession::new(AgentSessionConfig {
        agent: maho_agent::Agent::new(maho_agent::AgentOptions {
            initial_state: Some(maho_agent::agent::PartialAgentState {
                model: Some(model),
                ..Default::default()
            }),
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
        session_manager: session_manager(&cwd),
        settings_manager: maho_core::settings_manager::SettingsManager::from_storage(
            Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),
            false,
        ),
        cwd: cwd.clone(),
        agent_dir: Some(cwd),
        fallback_now: Some(Arc::new(|| 0.0)),
        retry_random: Some(Arc::new(|| 0.5)),
        scoped_models: Vec::new(),
        favorite_models: Vec::new(),
        flag_values: Default::default(),
        custom_tools: Vec::new(),
        model_runtime: Some(runtime),
        model_registry: None,
        uses_default_stream_function: Some(false),
        initial_active_tool_names: None,
        default_tool_names: None,
        eval_only_tool_names: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        session_start_event: None,
        auto_title_sessions: Some(false),
    })
    .expect("session");
    let session = Arc::new(session);
    let mode = maho_interactive::interactive_mode::InteractiveMode::new(
        session.clone(),
        maho_interactive::theme::Theme::builtin(
            "dark",
            maho_interactive::theme::ColorMode::Truecolor,
        )
        .expect("theme"),
        std::rc::Rc::new(EditorHost),
    );
    (mode, directory, session)
}

#[tokio::test]
async fn native_prompt_renders_one_user_card_and_one_completed_assistant() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            mode.submit("hi", Default::default())
        )
        .await
        .expect("bounded turn")
        .expect("prompt"),
        maho_core::agent_session::PromptDisposition::Started
    );
    assert!(mode.agent_idle);
    for width in [40, 80, 120] {
        let text = mode.render(width).join("\n");
        assert_eq!(text.matches("hi").count(), 1, "user echo at {width}");
        assert_eq!(text.matches("hello").count(), 1, "assistant at {width}");
    }
}

#[tokio::test]
async fn native_faux_events_include_user_and_final_assistant_before_idle() {
    let session = FauxSession::new(FauxScript {
        name: "interactive-native".into(),
        prompt: "hi".into(),
        responses: vec![FauxResponse {
            content: "hello".into(),
            stop_reason: "stop".into(),
        }],
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await
        .expect("bounded turn")
        .expect("native session");
    let events: Vec<maho_agent::types::AgentEvent> =
        serde_json::from_value(result["events"].clone()).expect("native typed events");
    assert!(matches!(
        events.first(),
        Some(maho_agent::types::AgentEvent::AgentStart)
    ));
    assert!(matches!(
        events.last(),
        Some(maho_agent::types::AgentEvent::AgentEnd { .. })
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, maho_agent::types::AgentEvent::MessageEnd { .. }))
            .count(),
        2
    );
}

#[test]
fn tool_lifecycle_updates_one_card_and_replaces_partial_result() {
    use maho_agent::types::AgentEvent;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.handle_event(&AgentEvent::ToolExecutionStart {
        parent_tool_call_id: None,
        tool_call_id: "call-1".into(),
        tool_name: "custom".into(),
        args: serde_json::json!({}),
    });
    mode.handle_event(&AgentEvent::ToolExecutionUpdate {
        parent_tool_call_id: None,
        tool_call_id: "call-1".into(),
        tool_name: "custom".into(),
        args: serde_json::json!({}),
        partial_result: serde_json::json!({"content":[{"type":"text","text":"partial-value"}]}),
    });
    assert!(mode.render(80).join("\n").contains("partial-value"));
    mode.handle_event(&AgentEvent::ToolExecutionEnd {
        parent_tool_call_id: None,
        tool_call_id: "call-1".into(),
        tool_name: "custom".into(),
        result: serde_json::json!({"content":[{"type":"text","text":"final-value"}]}),
        is_error: false,
    });
    let rendered = mode.render(80).join("\n");
    assert!(!rendered.contains("partial-value"));
    assert_eq!(rendered.matches("final-value").count(), 1);
}

#[test]
fn terminal_title_follows_the_running_tool_then_returns_to_the_normal_title() {
    use maho_agent::types::AgentEvent;
    let (mut mode, directory) = native_mode();
    let app_title = maho_core::config::app_title();
    let cwd_basename = directory
        .path()
        .file_name()
        .expect("cwd basename")
        .to_string_lossy()
        .into_owned();
    let normal = format!("{app_title} - {cwd_basename}");
    assert_eq!(mode.terminal_window_title(), normal);
    assert!(
        !mode.terminal_progress_active(),
        "showTerminalProgress defaults to off"
    );
    mode.handle_event(&AgentEvent::ToolExecutionStart {
        parent_tool_call_id: None,
        tool_call_id: "call-1".into(),
        tool_name: "bash".into(),
        args: serde_json::json!({"command":"ls"}),
    });
    assert_eq!(
        mode.terminal_window_title(),
        format!("{app_title} - Running bash: ls")
    );
    mode.handle_event(&AgentEvent::ToolExecutionEnd {
        parent_tool_call_id: None,
        tool_call_id: "call-1".into(),
        tool_name: "bash".into(),
        result: serde_json::json!({"content":[]}),
        is_error: false,
    });
    assert_eq!(mode.terminal_window_title(), normal);
}

#[test]
fn tool_end_without_start_still_renders_final_result() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.handle_event(&maho_agent::types::AgentEvent::ToolExecutionEnd {
        parent_tool_call_id: None,
        tool_call_id: "orphan".into(),
        tool_name: "custom".into(),
        result: serde_json::json!({"content":[{"type":"text","text":"failed-result"}]}),
        is_error: true,
    });
    assert!(mode.render(80).join("\n").contains("failed-result"));
}

#[tokio::test]
async fn enter_captures_editor_text_clears_draft_and_submits_native_prompt() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.handle_input("hi");
    mode.handle_input("\r");
    assert!(mode.editor.editor.get_text().is_empty());
    assert_eq!(
        mode.submit_editor().await.expect("submit"),
        Some(maho_core::agent_session::PromptDisposition::Started)
    );
    assert!(mode.render(80).join("\n").contains("hello"));
    assert_eq!(
        mode.submit_editor().await.expect("empty submission queue"),
        None
    );
}

async fn next_auth_lifecycle_event(
    mode: &mut maho_interactive::interactive_mode::InteractiveMode,
    events: &mut tokio::sync::broadcast::Receiver<
        maho_interactive::interactive_mode::AuthLifecycleEvent,
    >,
    mut wanted: impl FnMut(&maho_interactive::interactive_mode::AuthLifecycleEvent) -> bool,
) -> maho_interactive::interactive_mode::AuthLifecycleEvent {
    let mut observed = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            tokio::select! {
                event = events.recv() => {
                    let event = event.expect("authentication lifecycle channel");
                    if wanted(&event) { return event; }
                    observed.push(event);
                }
                event = mode.drain_next_auth_event() => {
                    event.expect("authentication event channel");
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("bounded authentication lifecycle event; observed: {observed:?}"))
}

#[tokio::test]
async fn login_api_key_saves_credential_and_logout_removes_it() {
    use maho_interactive::interactive_mode::{AuthLifecycleEvent, AuthLifecycleOutcome};
    use maho_tui::tui::Component;
    let (mut mode, directory) = native_mode();
    let auth_path = directory.path().join("auth.json");
    let mut events = mode.subscribe_auth_lifecycle();
    mode.enqueue_submission(
        &maho_interactive::interactive_mode::internal_command("/login-provider faux api_key"),
        None,
    );
    assert_eq!(
        mode.dispatch_submission().await.expect("login"),
        Some(maho_core::agent_session::PromptDisposition::Handled)
    );
    assert_eq!(
        next_auth_lifecycle_event(&mut mode, &mut events, |event| matches!(
            event,
            AuthLifecycleEvent::PromptReady
        ))
        .await,
        AuthLifecycleEvent::PromptReady
    );
    mode.handle_input("test-key");
    mode.handle_input("\r");
    assert!(
        matches!(next_auth_lifecycle_event(&mut mode, &mut events, |event| matches!(event, AuthLifecycleEvent::LoginCompleted { .. })).await,
        AuthLifecycleEvent::LoginCompleted { provider_name, method: maho_interactive::login_outcome::LoginMethod::ApiKey, outcome: AuthLifecycleOutcome::Success }
            if provider_name == "faux")
    );
    assert!(
        matches!(next_auth_lifecycle_event(&mut mode, &mut events, |event| matches!(event, AuthLifecycleEvent::LoginCatalogCompleted { .. })).await,
        AuthLifecycleEvent::LoginCatalogCompleted { provider_name, outcome: Ok(()), selection_error: None } if provider_name == "faux")
    );
    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&auth_path).expect("auth file"))
            .expect("auth json");
    assert_eq!(stored["faux"]["key"], "test-key");

    let mut events = mode.subscribe_auth_lifecycle();
    mode.enqueue_submission("/logout", None);
    assert_eq!(
        mode.dispatch_submission().await.expect("logout"),
        Some(maho_core::agent_session::PromptDisposition::Handled)
    );
    mode.poll_after_paint(std::task::Waker::noop());
    mode.handle_input("\r");
    assert_eq!(
        mode.dispatch_submission().await.expect("logout provider"),
        Some(maho_core::agent_session::PromptDisposition::Handled)
    );
    mode.poll_after_paint(std::task::Waker::noop());
    assert!(
        matches!(next_auth_lifecycle_event(&mut mode, &mut events, |event| matches!(event, AuthLifecycleEvent::LogoutCompleted { .. })).await,
        AuthLifecycleEvent::LogoutCompleted { provider_name, auth_type: maho_interactive::components::oauth_selector::AuthSelectorAuthType::ApiKey, outcome: Ok(()) }
            if provider_name == "faux")
    );
    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&auth_path).expect("auth file"))
            .expect("auth json");
    assert!(
        stored.get("faux").is_none(),
        "logout must remove the stored credential: {stored}"
    );
}

#[tokio::test]
async fn login_cached_default_is_selected_before_catalog_refresh_completes() {
    use maho_interactive::interactive_mode::{AuthLifecycleEvent, AuthLifecycleOutcome};
    use maho_tui::tui::Component;
    let (mut mode, _directory, session) = native_mode_with_session_and_provider_retaining_session(
        None,
        |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
        "anthropic",
        "claude-opus-4-8",
        true,
    );
    let session_id = session.session_id();
    let mut events = mode.subscribe_auth_lifecycle();
    mode.enqueue_submission(
        &maho_interactive::interactive_mode::internal_command("/login-provider anthropic api_key"),
        None,
    );
    assert_eq!(
        mode.dispatch_submission().await.expect("login"),
        Some(maho_core::agent_session::PromptDisposition::Handled)
    );
    assert_eq!(
        next_auth_lifecycle_event(&mut mode, &mut events, |event| matches!(
            event,
            AuthLifecycleEvent::PromptReady
        ))
        .await,
        AuthLifecycleEvent::PromptReady
    );
    mode.handle_input("test-key");
    mode.handle_input("\r");
    assert!(
        matches!(next_auth_lifecycle_event(&mut mode, &mut events, |event| matches!(event, AuthLifecycleEvent::LoginCompleted { .. })).await,
        AuthLifecycleEvent::LoginCompleted { provider_name, method: maho_interactive::login_outcome::LoginMethod::ApiKey, outcome: AuthLifecycleOutcome::Success } if provider_name == "anthropic")
    );
    assert_eq!(session.model().provider, "anthropic");
    assert_eq!(session.model().id, "claude-opus-4-8");
    assert_eq!(session.session_id(), session_id);
    assert!(matches!(
        next_auth_lifecycle_event(&mut mode, &mut events, |event| matches!(
            event,
            AuthLifecycleEvent::LoginCatalogCompleted { .. }
        ))
        .await,
        AuthLifecycleEvent::LoginCatalogCompleted {
            outcome: Ok(()),
            selection_error: None,
            ..
        }
    ));
    assert_eq!(session.model().provider, "anthropic");
    assert_eq!(session.model().id, "claude-opus-4-8");
    assert_eq!(session.session_id(), session_id);
}

#[tokio::test]
async fn login_catalog_completion_does_not_replace_a_new_session_model() {
    use maho_interactive::interactive_mode::{AuthLifecycleEvent, AuthLifecycleOutcome};
    use maho_tui::tui::Component;
    let (mut mode, _directory, session) = native_mode_with_session_and_provider_retaining_session(
        None,
        |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
        "anthropic",
        "claude-opus-4-8",
        true,
    );
    let mut events = mode.subscribe_auth_lifecycle();
    mode.enqueue_submission(
        &maho_interactive::interactive_mode::internal_command("/login-provider anthropic api_key"),
        None,
    );
    assert_eq!(
        mode.dispatch_submission().await.expect("login"),
        Some(maho_core::agent_session::PromptDisposition::Handled)
    );
    assert_eq!(
        next_auth_lifecycle_event(&mut mode, &mut events, |event| matches!(
            event,
            AuthLifecycleEvent::PromptReady
        ))
        .await,
        AuthLifecycleEvent::PromptReady
    );
    mode.handle_input("test-key");
    mode.handle_input("\r");
    assert!(matches!(
        next_auth_lifecycle_event(&mut mode, &mut events, |event| matches!(
            event,
            AuthLifecycleEvent::LoginCompleted { .. }
        ))
        .await,
        AuthLifecycleEvent::LoginCompleted {
            outcome: AuthLifecycleOutcome::Success,
            ..
        }
    ));
    session.new_session(None).await.expect("new session");
    let new_session_id = session.session_id();
    let new_model = session.model();
    assert!(matches!(
        next_auth_lifecycle_event(&mut mode, &mut events, |event| matches!(
            event,
            AuthLifecycleEvent::LoginCatalogCompleted { .. }
        ))
        .await,
        AuthLifecycleEvent::LoginCatalogCompleted {
            outcome: Ok(()),
            selection_error: None,
            ..
        }
    ));
    assert_eq!(session.session_id(), new_session_id);
    assert_eq!(session.model().provider, new_model.provider);
    assert_eq!(session.model().id, new_model.id);
}

#[tokio::test]
async fn login_cancel_shows_neutral_status_and_leaves_auth_unchanged() {
    use maho_interactive::interactive_mode::{AuthLifecycleEvent, AuthLifecycleOutcome};
    use maho_tui::tui::Component;
    let (mut mode, directory) = native_mode();
    let auth_path = directory.path().join("auth.json");
    let before = std::fs::read_to_string(&auth_path).ok();
    let mut events = mode.subscribe_auth_lifecycle();
    mode.enqueue_submission(
        &maho_interactive::interactive_mode::internal_command("/login-provider faux api_key"),
        None,
    );
    assert_eq!(
        mode.dispatch_submission().await.expect("login"),
        Some(maho_core::agent_session::PromptDisposition::Handled)
    );
    assert_eq!(
        next_auth_lifecycle_event(&mut mode, &mut events, |event| matches!(
            event,
            AuthLifecycleEvent::PromptReady
        ))
        .await,
        AuthLifecycleEvent::PromptReady
    );
    mode.handle_input("\u{1b}");
    assert!(
        matches!(next_auth_lifecycle_event(&mut mode, &mut events, |event| matches!(event, AuthLifecycleEvent::LoginCompleted { .. })).await,
        AuthLifecycleEvent::LoginCompleted { provider_name, method: maho_interactive::login_outcome::LoginMethod::ApiKey, outcome: AuthLifecycleOutcome::Cancelled }
            if provider_name == "faux")
    );
    assert_eq!(std::fs::read_to_string(&auth_path).ok(), before);
}

#[tokio::test]
async fn empty_enter_does_not_start_provider_turn() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.handle_input("\r");
    assert_eq!(mode.submit_editor().await.expect("empty"), None);
    assert!(mode.agent_idle);
}

#[tokio::test]
async fn rename_command_updates_session_without_consuming_provider_response() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    assert_eq!(
        mode.submit("/rename named-session", Default::default())
            .await
            .expect("rename"),
        maho_core::agent_session::PromptDisposition::Handled
    );
    assert!(mode.render(80).join("\n").contains("named-session"));
    assert_eq!(
        mode.submit("hi", Default::default())
            .await
            .expect("native turn"),
        maho_core::agent_session::PromptDisposition::Started
    );
    assert!(mode.render(80).join("\n").contains("hello"));
}

#[tokio::test]
async fn invalid_thinking_command_does_not_become_provider_input() {
    let (mut mode, _directory) = native_mode();
    assert!(
        mode.submit("/thinking nonsense", Default::default())
            .await
            .is_err()
    );
    assert!(mode.agent_idle);
    assert_eq!(
        mode.submit("hi", Default::default())
            .await
            .expect("native turn"),
        maho_core::agent_session::PromptDisposition::Started
    );
}

#[tokio::test]
async fn rename_without_argument_opens_real_input_and_commits_enter() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    assert_eq!(
        mode.submit("/rename", Default::default())
            .await
            .expect("dialog"),
        maho_core::agent_session::PromptDisposition::Handled
    );
    mode.handle_input("dialog-name");
    mode.handle_input("\r");
    assert!(mode.render(80).join("\n").contains("dialog-name"));
    assert!(mode.editor.editor.get_text().is_empty());
}

#[tokio::test]
async fn rename_warns_when_the_session_normalizes_the_name_and_skips_an_unchanged_commit() {
    use maho_tui::tui::Component;
    let (mut mode, _directory, session) = native_mode_with_session_and_provider_retaining_session(
        None,
        |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
        "faux",
        "faux-1",
        false,
    );
    mode.submit("/rename a\nb", Default::default())
        .await
        .expect("rename");
    assert!(
        mode.render(80)
            .join("\n")
            .contains(r#"Session name was normalized from "a\nb" to "a b""#)
    );
    let entries = || {
        session.with_session_manager(|manager| {
            manager
                .entries()
                .iter()
                .filter(|entry| {
                    entry.get("type").and_then(serde_json::Value::as_str) == Some("session_info")
                })
                .count()
        })
    };
    let before = entries();
    mode.submit("/rename", Default::default())
        .await
        .expect("dialog");
    mode.handle_input("\r");
    assert_eq!(
        entries(),
        before,
        "confirming the unchanged name appends no session_info entry"
    );
}

#[tokio::test]
async fn native_footer_reflects_session_name_and_selected_model() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("/name footer-name", Default::default())
        .await
        .expect("rename");
    let snapshot = mode.footer_snapshot();
    assert_eq!(snapshot.session_name.as_deref(), Some("footer-name"));
    assert_eq!(snapshot.model_id.as_deref(), Some("faux-1"));
    let lines = mode.render(120);
    assert!(lines.last().expect("footer").contains("faux-1"));
}

#[test]
fn ctrl_c_clear_then_second_press_requests_shutdown_without_timing_luck() {
    let (mut mode, _directory) = native_mode();
    mode.editor.editor.set_text("draft");
    mode.handle_input_at("\x03", 1000);
    assert!(mode.editor.editor.get_text().is_empty());
    assert!(!mode.shutdown_requested);
    mode.handle_input_at("\x03", 1499);
    assert!(mode.shutdown_requested);
}

#[test]
fn question_opens_shortcut_overlay_and_next_key_dismisses_while_typing() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let before = mode.render(80).len();
    mode.handle_input_at("?", 0);
    assert_eq!(mode.editor.editor.get_text(), "?");
    assert!(mode.render(80).len() > before);
    mode.handle_input_at("a", 1);
    assert_eq!(mode.editor.editor.get_text(), "?a");
    assert_eq!(mode.render(80).len(), before);
}

#[tokio::test]
async fn jsonl_export_preserves_native_messages_and_quoted_path() {
    let (mut mode, directory) = native_mode();
    mode.submit("hi", Default::default()).await.expect("prompt");
    let path = directory.path().join("export with spaces.jsonl");
    assert_eq!(
        mode.submit(
            &format!("/export \"{}\"", path.display()),
            Default::default()
        )
        .await
        .expect("export"),
        maho_core::agent_session::PromptDisposition::Handled
    );
    let entries = std::fs::read_to_string(path)
        .expect("export")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("jsonl"))
        .collect::<Vec<_>>();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry["type"] == "message")
            .count(),
        2
    );
}

#[test]
fn command_path_parser_preserves_quotes_and_first_argument_semantics() {
    use maho_interactive::interactive_mode::get_path_command_argument;
    assert_eq!(
        get_path_command_argument("/export 'a b.jsonl' ignored", "/export").as_deref(),
        Some("a b.jsonl")
    );
    assert_eq!(
        get_path_command_argument("/export a.jsonl ignored", "/export").as_deref(),
        Some("a.jsonl")
    );
    assert_eq!(
        get_path_command_argument("/export 'unclosed", "/export"),
        None
    );
}

#[tokio::test]
async fn extension_select_uses_real_selector_and_returns_enter_choice() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let choices = vec!["first".into(), "second".into()];
    let selected = ui.select("Pick", &choices, Default::default());
    mode.render(80);
    mode.handle_input_at("\x1b[B", 0);
    mode.handle_input_at("\r", 1);
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), selected)
            .await
            .expect("bounded selector"),
        Some("second".into())
    );
}

#[tokio::test]
async fn extension_input_uses_real_input_and_returns_typed_value() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let answer = ui.input("Name", None, Default::default());
    mode.render(80);
    mode.handle_input_at("typed-value", 0);
    mode.handle_input_at("\r", 1);
    assert_eq!(answer.await.as_deref(), Some("typed-value"));
}

#[test]
fn extension_editor_widgets_and_status_reach_native_surface() {
    use maho_ext_api::{ExtensionUi, WidgetContent};
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.extension_ui.set_editor_text("extension-draft");
    mode.extension_ui.set_widget(
        "fixture",
        Some(WidgetContent::Lines(vec!["widget-value".into()])),
        Default::default(),
    );
    mode.extension_ui
        .set_status("fixture", Some("extension-status"));
    let lines = mode.render(120).join("\n");
    assert_eq!(mode.extension_ui.get_editor_text(), "extension-draft");
    assert!(lines.contains("widget-value"));
    assert!(lines.contains("extension-status"));
    mode.extension_ui
        .set_widget("fixture", None, Default::default());
    assert!(!mode.render(120).join("\n").contains("widget-value"));
}

#[test]
fn assistant_text_segments_remain_on_either_side_of_tool_card() {
    use maho_agent::types::AgentEvent;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let message: maho_agent::types::AgentMessage = serde_json::from_value(serde_json::json!({"role":"assistant", "content":[{"type":"text","text":"before-tool"},{"type":"toolCall","id":"ordered","name":"custom","arguments":{}},{"type":"text","text":"after-tool"}], "api":"faux", "provider":"faux", "model":"faux-1", "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},"stopReason":"toolUse","timestamp":0})).expect("message");
    mode.handle_event(&AgentEvent::MessageStart {
        message: message.clone(),
    });
    mode.handle_event(&AgentEvent::MessageEnd { message });
    mode.handle_event(&AgentEvent::ToolExecutionEnd {
        parent_tool_call_id: None,
        tool_call_id: "ordered".into(),
        tool_name: "custom".into(),
        result: serde_json::json!({"content":[{"type":"text","text":"tool-output"}]}),
        is_error: false,
    });
    let lines = mode.render(80).join("\n");
    assert!(lines.find("before-tool").expect("head") < lines.find("tool-output").expect("tool"));
    assert!(lines.find("tool-output").expect("tool") < lines.find("after-tool").expect("tail"));
    assert_eq!(lines.matches("after-tool").count(), 1);
}

#[test]
fn streamed_tool_arguments_finish_with_exact_parsed_arguments() {
    use maho_agent::types::AgentEvent;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let message: maho_agent::types::AgentMessage = serde_json::from_value(serde_json::json!({"role":"assistant", "content":[{"type":"toolCall","id":"arguments","name":"custom","arguments":{"value":"exact-final"}}], "api":"faux", "provider":"faux", "model":"faux-1", "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},"stopReason":"toolUse","timestamp":0})).expect("message");
    mode.handle_event(&AgentEvent::AgentStart);
    mode.handle_event(&AgentEvent::MessageStart {
        message: message.clone(),
    });
    mode.handle_event(&AgentEvent::MessageUpdate {
        message: message.clone(),
        assistant_message_event: maho_ai::types::AssistantMessageEvent::ToolcallDelta {
            content_index: 0,
            delta: "{\"value\":\"partial".into(),
            partial: message.as_assistant().expect("assistant").clone(),
        },
    });
    mode.handle_event(&AgentEvent::MessageEnd { message });
    mode.tick(1000.0);
    assert!(mode.render(80).join("\n").contains("exact-final"));
}

#[test]
fn plain_string_user_content_renders_in_native_transcript() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let message = serde_json::from_value(
        serde_json::json!({"role":"user","content":"string-user-message","timestamp":0}),
    )
    .expect("user message");
    mode.handle_event(&maho_agent::types::AgentEvent::MessageStart { message });
    assert!(mode.render(80).join("\n").contains("string-user-message"));
}

#[tokio::test]
async fn registered_extension_markdown_transformer_reaches_native_assistant() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let mut api = maho_ext_api::ExtensionApi::new(
        maho_ext_api::LoadedExtension::new("native-fixture", "/tmp".into(), Default::default()),
        Default::default(),
        Default::default(),
        Default::default(),
    );
    api.register_markdown_transformer(std::sync::Arc::new(|text, context| {
        if context.message_type == maho_ext_api::MarkdownMessageType::Assistant {
            text.replace("hello", "transformed-reply")
        } else {
            text.into()
        }
    }));
    mode.use_registered_markdown_transformers(&[api.registered]);
    mode.submit("hi", Default::default())
        .await
        .expect("native turn");
    let lines = mode.render(80).join("\n");
    assert!(lines.contains("transformed-reply"));
    assert!(!lines.contains("hello"));
}

#[tokio::test]
async fn rebuilding_native_history_does_not_duplicate_existing_transcript() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("hi", Default::default()).await.expect("turn");
    mode.rebuild_history();
    mode.rebuild_history();
    assert_eq!(mode.render(80).join("\n").matches("hello").count(), 1);
}

#[test]
fn hidden_custom_history_is_not_rendered() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    for display in [false, true] {
        let message =
            maho_agent::types::AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(
                maho_agent::harness::messages::CustomMessage {
                    role: "custom".into(),
                    custom_type: "fixture".into(),
                    content: maho_agent::harness::messages::CustomMessageContent::Text(
                        if display {
                            "visible-value"
                        } else {
                            "hidden-value"
                        }
                        .into(),
                    ),
                    display,
                    details: None,
                    timestamp: 0,
                },
            ));
        mode.add_history_message(&message);
    }
    let rendered = mode.render(80).join("\n");
    assert!(rendered.contains("visible-value"));
    assert!(!rendered.contains("hidden-value"));
}

#[tokio::test]
async fn queued_messages_restore_in_enqueue_order_ahead_of_live_draft() {
    let (mut mode, _directory) = native_mode();
    mode.steer("first").await.expect("steer");
    mode.follow_up("second").await.expect("follow up");
    mode.editor.editor.set_text("draft");
    assert_eq!(mode.restore_queued_messages(false), 2);
    assert_eq!(mode.editor.editor.get_text(), "first\n\nsecond\n\ndraft");
    assert_eq!(mode.restore_queued_messages(false), 0);
}

#[tokio::test]
async fn smooth_reveal_ticks_show_buffered_text_and_final_event_flushes_it() {
    use maho_tui::tui::Component;
    let result = FauxSession::new(FauxScript {
        name: "pacing".into(),
        prompt: "hi".into(),
        responses: vec![FauxResponse {
            content: "paced-reply".into(),
            stop_reason: "stop".into(),
        }],
    })
    .run_native()
    .await
    .expect("native");
    let message: maho_agent::types::AgentMessage =
        serde_json::from_value(result["messages"][1].clone()).expect("assistant");
    let assistant = message.as_assistant().expect("assistant").clone();
    let (mut mode, _directory) = native_mode();
    mode.handle_event(&maho_agent::types::AgentEvent::MessageStart {
        message: message.clone(),
    });
    mode.handle_event(&maho_agent::types::AgentEvent::MessageUpdate {
        message: message.clone(),
        assistant_message_event: maho_ai::types::AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "paced-reply".into(),
            partial: assistant,
        },
    });
    for frame in 100..200 {
        mode.tick(f64::from(frame) * 100.0);
    }
    assert!(mode.render(80).join("\n").contains("paced-reply"));
    mode.handle_event(&maho_agent::types::AgentEvent::MessageEnd { message });
    assert_eq!(mode.render(80).join("\n").matches("paced-reply").count(), 1);
}

#[tokio::test]
async fn session_command_reports_native_turn_without_starting_another() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("hi", Default::default()).await.expect("turn");
    assert_eq!(
        mode.submit("/session", Default::default())
            .await
            .expect("stats"),
        maho_core::agent_session::PromptDisposition::Handled
    );
    assert!(mode.render(80).join("\n").contains("In-memory"));
    assert!(mode.agent_idle);
}

#[test]
fn consecutive_extension_notifications_replace_status_not_append_it() {
    use maho_ext_api::{ExtensionUi, NotificationType};
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.extension_ui
        .notify("first-status", NotificationType::Info);
    mode.render(80);
    mode.extension_ui
        .notify("second-status", NotificationType::Info);
    let lines = mode.render(80).join("\n");
    assert!(!lines.contains("first-status"));
    assert_eq!(lines.matches("second-status").count(), 1);
}

#[test]
fn widget_refresh_keeps_insertion_order_and_caps_lines() {
    use maho_ext_api::{ExtensionUi, WidgetContent};
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.extension_ui.set_widget(
        "z",
        Some(WidgetContent::Lines(vec!["first-widget".into()])),
        Default::default(),
    );
    mode.extension_ui.set_widget(
        "a",
        Some(WidgetContent::Lines(vec!["second-widget".into()])),
        Default::default(),
    );
    mode.render(80);
    mode.extension_ui.set_widget(
        "z",
        Some(WidgetContent::Lines(
            (0..12).map(|n| format!("updated-widget-{n}")).collect(),
        )),
        Default::default(),
    );
    let lines = mode.render(80).join("\n");
    assert!(
        lines.find("updated-widget-0").expect("first")
            < lines.find("second-widget").expect("second")
    );
    assert!(!lines.contains("updated-widget-11"));
}

#[tokio::test]
async fn cancelled_extension_input_releases_composer() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let signal = maho_tools::definition::AbortSignal::default();
    let answer = ui.input(
        "Cancelled",
        None,
        maho_ext_api::ExtensionUiDialogOptions {
            signal: Some(signal.clone()),
            ..Default::default()
        },
    );
    mode.render(80);
    signal.abort();
    assert_eq!(answer.await, None);
    mode.render(80);
    mode.handle_input_at("new-draft", 0);
    assert_eq!(mode.editor.editor.get_text(), "new-draft");
}

#[test]
fn expansion_updates_completed_tool_cards_not_just_pending_cards() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let text = (0..100)
        .map(|n| format!("line-{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    mode.handle_event(&maho_agent::types::AgentEvent::ToolExecutionEnd {
        parent_tool_call_id: None,
        tool_call_id: "expanded".into(),
        tool_name: "ls".into(),
        result: serde_json::json!({"content":[{"type":"text","text":text}]}),
        is_error: false,
    });
    assert!(!mode.render(80).join("\n").contains("line-99"));
    mode.set_tools_expanded(true);
    assert!(mode.render(80).join("\n").contains("line-99"));
}

#[tokio::test]
async fn bare_thinking_selector_filters_then_cancels_without_provider_turn() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("/thinking", Default::default())
        .await
        .expect("selector");
    mode.render(80);
    mode.handle_input_at("off", 0);
    mode.render(80);
    mode.handle_input_at("\x1b", 1);
    mode.handle_input_at("draft", 2);
    assert_eq!(mode.editor.editor.get_text(), "draft");
    assert_eq!(mode.footer_snapshot().context_tokens, Some(0.0));
}

#[test]
fn working_row_animates_until_exact_agent_end_event() {
    let (mut mode, _directory) = native_mode();
    assert!(mode.working_frame(0.0).is_none());
    mode.handle_event(&maho_agent::types::AgentEvent::AgentStart);
    assert_ne!(mode.working_frame(100.0), mode.working_frame(700.0));
    mode.handle_event(&maho_agent::types::AgentEvent::AgentEnd {
        messages: Vec::new(),
    });
    assert!(mode.working_frame(800.0).is_none());
}

#[test]
fn extension_working_visibility_controls_native_active_row() {
    use maho_ext_api::ExtensionUi;
    let (mut mode, _directory) = native_mode();
    mode.handle_event(&maho_agent::types::AgentEvent::AgentStart);
    mode.extension_ui.set_working_visible(false).expect("hide");
    mode.drain_events();
    assert!(mode.working_frame(100.0).is_none());
    mode.extension_ui.set_working_visible(true).expect("show");
    mode.drain_events();
    assert!(mode.working_frame(100.0).is_some());
}

#[tokio::test]
async fn extension_editor_returns_prefill_through_real_multiline_component() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let result = ui.editor("Edit", Some("prefill"));
    mode.render(80);
    mode.handle_input_at("\r", 0);
    assert_eq!(result.await.expect("editor"), Some("prefill".into()));
}

#[tokio::test]
async fn extension_editor_ctrl_g_requests_a_terminal_hand_off() {
    use maho_ext_api::ExtensionUi;
    use maho_interactive::interactive_mode::TerminalRequest;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let _result = ui.editor("Edit", Some("prefill"));
    mode.render(80);
    mode.handle_input_at("\x07", 0);
    assert_eq!(
        mode.take_terminal_request(),
        Some(TerminalRequest::ExternalEditor)
    );
}

#[test]
fn extension_terminal_input_transforms_consumes_and_unsubscribes() {
    use maho_ext_api::{ExtensionUi, TerminalInputResult};
    let (mut mode, _directory) = native_mode();
    let unsubscribe = mode
        .extension_ui
        .on_terminal_input(std::sync::Arc::new(|data| {
            Some(TerminalInputResult {
                consume: Some(data == "blocked"),
                data: Some("replacement".into()),
            })
        }))
        .expect("listener");
    mode.handle_input_at("blocked", 0);
    assert!(mode.editor.editor.get_text().is_empty());
    mode.handle_input_at("original", 1);
    assert_eq!(mode.editor.editor.get_text(), "replacement");
    unsubscribe();
    mode.editor.editor.set_text("");
    mode.handle_input_at("original", 2);
    assert_eq!(mode.editor.editor.get_text(), "original");
}

#[tokio::test]
async fn unintegrated_builtin_does_not_silently_become_provider_input() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    assert_eq!(
        mode.submit("/compact", Default::default())
            .await
            .expect("compact is handled with the pinned warning"),
        maho_core::agent_session::PromptDisposition::Handled
    );
    assert!(
        mode.render(80)
            .join("\n")
            .contains("Nothing to compact (no messages yet)")
    );
    assert_eq!(
        mode.footer_snapshot().context_tokens,
        Some(0.0),
        "no command became a provider turn"
    );
}

#[tokio::test]
async fn html_export_writes_the_document_without_a_shared_host() {
    use maho_tui::tui::Component;
    let (mut mode, directory) = native_mode_with_session(None, |cwd| {
        let session_dir = std::path::Path::new(cwd).join("sessions");
        std::fs::create_dir_all(&session_dir).expect("session dir");
        maho_core::session_manager::SessionManager::create(
            cwd,
            Some(&session_dir.to_string_lossy()),
            None,
        )
    });
    mode.submit("hi", Default::default()).await.expect("prompt");
    let path = directory.path().join("session.html");
    assert_eq!(
        mode.submit(&format!("/export {}", path.display()), Default::default())
            .await
            .expect("html export"),
        maho_core::agent_session::PromptDisposition::Handled
    );
    assert!(
        std::fs::read_to_string(&path)
            .expect("html document")
            .contains("id=\"session-data\"")
    );
    assert!(mode.render(80).join("\n").contains("Session exported to:"));
}

#[test]
fn extension_expansion_and_keyboard_expansion_share_state() {
    use maho_ext_api::ExtensionUi;
    let (mut mode, _directory) = native_mode();
    mode.extension_ui.set_tools_expanded(true).expect("expand");
    mode.drain_events();
    assert!(mode.tools_expanded);
    mode.handle_input_at("\x0f", 0);
    assert!(!mode.extension_ui.get_tools_expanded().expect("read"));
}

#[test]
fn question_answer_history_uses_existing_collapsed_answer_chip() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let message: maho_agent::types::AgentMessage = serde_json::from_value(serde_json::json!({"role":"user","content":[{"type":"text","text":"[Answer to question request-1]\nThe user did not answer"}],"timestamp":1})).expect("message");
    mode.add_history_message(&message);
    let lines = mode.render(80).join("\n");
    assert!(lines.contains("request-1"));
    assert!(lines.contains("(no answer)"));
    assert!(!lines.contains("[Answer to question"));
}

#[tokio::test]
async fn armin_says_hi_command_renders_the_easter_egg_instead_of_prompting() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    assert_eq!(
        mode.submit("/arminsayshi", Default::default())
            .await
            .expect("handled"),
        maho_core::agent_session::PromptDisposition::Handled
    );
    assert!(mode.render(80).join("\n").contains("ARMIN SAYS HI"));
    assert_eq!(
        mode.footer_snapshot().context_tokens,
        Some(0.0),
        "the command never became a provider turn"
    );
}

#[tokio::test]
async fn selecting_opencode_kimi_k2_5_shows_the_daxnuts_card() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode_with_provider(None, "opencode", "kimi-k2.5", false);
    mode.submit(
        &maho_interactive::interactive_mode::internal_command("/model-select opencode/kimi-k2.5"),
        Default::default(),
    )
    .await
    .expect("select");
    mode.tick(1_000_000.0);
    assert!(
        mode.render(80)
            .join("\n")
            .contains("Free Kimi K2.5 via OpenCode Zen")
    );
}

#[tokio::test]
async fn demented_delves_command_renders_the_earendil_announcement() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    assert_eq!(
        mode.submit("/dementedelves", Default::default())
            .await
            .expect("handled"),
        maho_core::agent_session::PromptDisposition::Handled
    );
    assert!(
        mode.render(80)
            .join("\n")
            .contains("pi has joined Earendil")
    );
    assert_eq!(
        mode.footer_snapshot().context_tokens,
        Some(0.0),
        "the command never became a provider turn"
    );
}

#[test]
fn startup_model_warnings_flag_a_risky_main_model() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode_with_provider(None, "qwen-cloud", "qwen-1", false);
    mode.show_startup_model_warnings();
    assert!(mode.render(80).join("\n").contains("Risky model warning"));
    let (mut safe, _safe_directory) = native_mode();
    safe.show_startup_model_warnings();
    assert!(!safe.render(80).join("\n").contains("Risky model warning"));
}

fn continuity_assistant(kind: &str) -> maho_agent::types::AgentMessage {
    serde_json::from_value(serde_json::json!({"role":"assistant", "content":[{"type":"text","text":"reply"}], "api":"faux", "provider":"faux", "model":"faux-1", "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},"stopReason":"stop","timestamp":0,
        "diagnostics":[{"type":"claude_sdk_oauth_session_continuity","timestamp":0,"details":{"kind":kind}}]})).expect("message")
}

#[test]
fn continuity_notice_renders_after_a_live_assistant_message_end() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let message = continuity_assistant("flatten");
    mode.handle_event(&maho_agent::types::AgentEvent::MessageStart {
        message: message.clone(),
    });
    mode.handle_event(&maho_agent::types::AgentEvent::MessageEnd { message });
    assert!(
        mode.render(80)
            .join("\n")
            .contains("Session continuity lost - resent the full conversation")
    );
}

#[test]
fn continuity_notice_is_rederived_on_history_replay_and_rebuild_resets_suppression() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.add_history_message(&continuity_assistant("disabled"));
    mode.add_history_message(&continuity_assistant("disabled"));
    assert_eq!(
        mode.render(80)
            .join("\n")
            .matches("Session continuity disabled")
            .count(),
        1,
        "the disabled notice renders once per transcript"
    );
    mode.rebuild_history();
    mode.add_history_message(&continuity_assistant("disabled"));
    assert_eq!(
        mode.render(80)
            .join("\n")
            .matches("Session continuity disabled")
            .count(),
        1,
        "rebuild resets the tracker so the notice reappears"
    );
}

#[tokio::test]
async fn new_command_replaces_native_session_and_clears_transcript() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("old-message", Default::default())
        .await
        .expect("turn");
    mode.submit("/new", Default::default()).await.expect("new");
    assert!(!mode.render(80).join("\n").contains("old-message"));
    assert_eq!(mode.footer_snapshot().context_tokens, Some(0.0));
}

#[tokio::test]
async fn new_command_confirms_with_the_accent_started_line() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("/new", Default::default()).await.expect("new");
    assert!(mode.render(80).join("\n").contains("✓ New session started"));
}

#[tokio::test]
async fn copy_without_agent_messages_renders_an_error_line() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    assert_eq!(
        mode.submit("/copy", Default::default())
            .await
            .expect("handled"),
        maho_core::agent_session::PromptDisposition::Handled
    );
    assert!(
        mode.render(80)
            .join("\n")
            .contains("Error: No agent messages to copy yet.")
    );
}

#[tokio::test]
async fn session_info_does_not_replace_the_previous_status_line() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.set_tools_expanded(true);
    mode.submit("/session", Default::default())
        .await
        .expect("session info");
    let rendered = mode.render(80).join("\n");
    assert!(
        rendered.contains("Tool output: expanded"),
        "the previous status survives: {rendered}"
    );
    assert!(rendered.contains("Session Info"));
}

#[tokio::test]
async fn reload_reports_the_upstream_resource_message() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    assert_eq!(
        mode.submit("/reload", Default::default())
            .await
            .expect("reload"),
        maho_core::agent_session::PromptDisposition::Handled
    );
    // senpi `Text(msg, 1, 0)` pads one column per side, so the 80-column message only fits unwrapped at >= 82.
    assert!(
        mode.render(120).join("\n").contains(
            "Reloading keybindings, extensions, skills, prompts, themes, and context files..."
        ),
        "the reload box replaces the editor first"
    );
    assert_eq!(
        mode.submit_editor().await.expect("queued reload"),
        Some(maho_core::agent_session::PromptDisposition::Handled)
    );
    let rendered = mode.render(80).join("\n");
    assert!(
        !rendered.contains("Reloading keybindings"),
        "the reload box is dismissed"
    );
    assert!(
        rendered.contains(
            "Reloaded keybindings, extensions, skills, prompts, themes, and context files"
        )
    );
}

#[tokio::test]
async fn reload_status_survives_the_live_loop_theme_detection_settling() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("/reload", Default::default())
        .await
        .expect("reload");
    mode.submit_editor().await.expect("queued reload");
    // The live loop polls the terminal theme every frame; `/reload` re-requested detection, so the
    // next poll queries OSC 11 and the reply settles it on the already-active dark theme.
    let theme = maho_interactive::theme::Theme::builtin(
        "dark",
        maho_interactive::theme::ColorMode::Truecolor,
    )
    .expect("theme");
    let mut ui = maho_interactive::tui_renderer::create_interactive_tui(
        maho_interactive::tui_renderer::InteractiveTuiOptions {
            tui_mode: maho_interactive::tui_renderer::TuiMode::Regular,
            show_hardware_cursor: false,
            bottom_shortcut: String::new(),
        },
        theme,
    );
    let mut terminal = ScreenTerminal {
        writes: String::new(),
        screen: maho_test_support::vterm::VirtualTerminal::new(120, 36),
        input: None,
        stopped: false,
    };
    mode.poll_terminal_theme(&mut ui, &mut terminal);
    assert!(
        terminal.writes.contains("\x1b]11;?"),
        "the reload re-queries the terminal background"
    );
    ui.base_mut()
        .handle_terminal_input("\x1b]11;rgb:0000/0000/0000\x07", false);
    mode.poll_terminal_theme(&mut ui, &mut terminal);
    mode.drain_events();
    let rendered = mode.render(120).join("\n");
    assert!(
        !rendered.contains("Reloading keybindings"),
        "the reload box is dismissed"
    );
    assert!(
        rendered.contains(
            "Reloaded keybindings, extensions, skills, prompts, themes, and context files"
        ),
        "senpi keeps the reload status visible: {rendered}"
    );
}

#[tokio::test]
async fn empty_compact_shows_the_pinned_no_messages_warning() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    assert_eq!(
        mode.submit("/compact", Default::default())
            .await
            .expect("handled"),
        maho_core::agent_session::PromptDisposition::Handled
    );
    assert!(
        mode.render(80)
            .join("\n")
            .contains("Nothing to compact (no messages yet)"),
        "the pinned interactive warning replaces the core error"
    );
    assert_eq!(mode.footer_snapshot().context_tokens, Some(0.0));
}

#[tokio::test]
async fn compaction_and_retry_events_mount_status_indicators() {
    use maho_ext_api::{AgentSessionEvent, CompactionReason};
    use maho_tui::{tui::Component, utils::strip_terminal_sequences};
    let (mut mode, _directory) = native_mode();
    mode.handle_event(&maho_agent::types::AgentEvent::AgentStart);
    assert!(
        strip_terminal_sequences(&mode.render(120).join("\n")).contains("Working ("),
        "working status renders in the editor border before compaction"
    );
    mode.handle_session_event(&AgentSessionEvent::CompactionStart {
        reason: CompactionReason::Threshold,
        request_id: None,
    });
    let compacting_frame = strip_terminal_sequences(&mode.render(120).join("\n"));
    assert!(
        compacting_frame.contains("Auto-compacting... ("),
        "compaction shows the reasoned loader with its interrupt hint"
    );
    assert!(
        !compacting_frame.contains("Working ("),
        "compaction suppresses the embedded working indicator so both never render together: {compacting_frame}"
    );
    assert!(
        mode.working_frame(100.0).is_none(),
        "working_frame is suppressed while compaction owns the status indicator"
    );
    mode.handle_session_event(&AgentSessionEvent::CompactionEnd {
        reason: CompactionReason::Threshold,
        result: None,
        aborted: false,
        will_retry: false,
        request_id: None,
        accepted: Some(true),
        rejection_cause: None,
        error_message: None,
    });
    let resumed_frame = strip_terminal_sequences(&mode.render(120).join("\n"));
    assert!(
        !resumed_frame.contains("Auto-compacting"),
        "compaction end clears the indicator"
    );
    assert!(
        resumed_frame.contains("Working ("),
        "working status returns to the editor border when the turn is still active"
    );
    mode.handle_session_event(&AgentSessionEvent::AutoRetryStart {
        attempt: 1,
        max_attempts: 3,
        delay_ms: 2000,
        error_message: "boom".into(),
    });
    let retry_frame = strip_terminal_sequences(&mode.render(120).join("\n"));
    assert!(
        retry_frame.contains("Retrying (1/3) in 2s..."),
        "auto-retry shows the countdown indicator in the editor border"
    );
    assert!(
        !retry_frame.contains("Working ("),
        "auto-retry replaces the working indicator in the editor border"
    );
    mode.handle_session_event(&AgentSessionEvent::AutoRetryEnd {
        success: true,
        attempt: 1,
        final_error: None,
    });
    assert!(
        !strip_terminal_sequences(&mode.render(120).join("\n")).contains("Retrying (1/3)"),
        "auto-retry end clears the indicator"
    );
}

#[tokio::test]
async fn idle_mode_skips_frame_rebuilds_until_a_state_change() {
    use maho_ext_api::AgentSessionEvent;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    // The host loop renders every frame that asks for a rebuild; transcript warm-up advances on paint.
    let frame = |mode: &mut maho_interactive::interactive_mode::InteractiveMode, now_ms: f64| {
        let rebuild = mode.take_frame_rebuild(now_ms);
        if rebuild { mode.render(120); }
        rebuild
    };
    assert!(frame(&mut mode, 0.0), "the first frame is rebuilt");
    assert!(!frame(&mut mode, 16.0), "an idle mode with no change asks for no rebuild");
    mode.handle_session_event(&AgentSessionEvent::AutoRetryStart { attempt: 1, max_attempts: 3, delay_ms: 2000, error_message: "boom".into() });
    assert!(frame(&mut mode, 32.0), "a session event asks for a rebuild");
    assert!(frame(&mut mode, 48.0), "the running retry countdown keeps animating");
    mode.handle_session_event(&AgentSessionEvent::AutoRetryEnd { success: true, attempt: 1, final_error: None });
    assert!(frame(&mut mode, 64.0), "the end event repaints the cleared indicator");
    assert!(!frame(&mut mode, 80.0), "idle again once the indicator is gone");
}

#[tokio::test]
async fn native_shell_submission_renders_and_persists_without_model_turn() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("!printf shell-output", Default::default())
        .await
        .expect("shell");
    assert!(mode.render(80).join("\n").contains("shell-output"));
    mode.rebuild_history();
    assert!(mode.render(80).join("\n").contains("shell-output"));
}

#[tokio::test]
async fn bare_model_selector_cancels_and_releases_native_composer() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("/model", Default::default())
        .await
        .expect("selector");
    mode.render(80);
    mode.handle_input_at("\x1b", 0);
    mode.handle_input_at("draft", 1);
    assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[tokio::test]
async fn model_command_switches_on_a_bare_exact_id() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    assert_eq!(
        mode.submit("/model faux-1", Default::default())
            .await
            .expect("model"),
        maho_core::agent_session::PromptDisposition::Handled
    );
    assert!(mode.render(80).join("\n").contains("Model: faux-1"));
}

#[tokio::test]
async fn model_command_miss_refreshes_then_opens_a_prefiltered_selector() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    assert_eq!(
        mode.submit("/model no-such-model", Default::default())
            .await
            .expect("model"),
        maho_core::agent_session::PromptDisposition::Handled
    );
    let rendered = mode.render(80).join("\n");
    assert!(
        rendered.contains("Refreshing model catalogs…"),
        "the catalog refresh precedes the selector: {rendered}"
    );
    // The focused search input draws its cursor (reverse video) on the first cell, so match plain text.
    let plain = maho_tools::ansi::strip_ansi(&rendered);
    assert!(
        plain.contains("> no-such-model"),
        "the selector opens with the term as its search: {plain}"
    );
    mode.handle_input_at("\x1b", 0);
    mode.handle_input_at("draft", 1);
    assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[tokio::test]
async fn resume_path_restores_exported_native_transcript() {
    use maho_tui::tui::Component;
    let (mut mode, directory) = native_mode();
    mode.submit("saved-message", Default::default())
        .await
        .expect("turn");
    let path = directory.path().join("resume.jsonl");
    mode.submit(&format!("/export {}", path.display()), Default::default())
        .await
        .expect("export");
    mode.submit("/new", Default::default()).await.expect("new");
    mode.submit(
        &maho_interactive::interactive_mode::internal_command(format!(
            "/resume {}",
            path.display()
        )),
        Default::default(),
    )
    .await
    .expect("resume");
    assert!(mode.render(80).join("\n").contains("saved-message"));
}

#[tokio::test]
async fn native_runtime_model_keys_do_not_insert_control_input() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.handle_runtime_input("\x10", 0).await.expect("cycle");
    assert!(mode.editor.editor.get_text().is_empty());
    mode.handle_runtime_input("\x0c", 1)
        .await
        .expect("selector");
    mode.render(80);
    mode.handle_runtime_input("\x1b", 2).await.expect("cancel");
    mode.handle_runtime_input("draft", 3).await.expect("type");
    assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[test]
fn native_editor_completes_builtin_command_from_canonical_catalog() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.editor.editor.set_text("/thi");
    mode.render(80);
    mode.handle_input_at("\t", 0);
    assert!(mode.editor.editor.is_showing_autocomplete());
    mode.handle_input_at("\t", 1);
    assert!(mode.editor.editor.get_text().starts_with("/thinking"));
}

#[test]
fn native_autocomplete_offers_skill_commands_only_while_enable_skill_commands_is_on() {
    use maho_tui::tui::Component;
    let (mut mode, _directory, session) = native_mode_with_session_and_provider_retaining_session(
        None,
        |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
        "faux",
        "faux-1",
        false,
    );
    session.set_prompt_resources(
        Vec::new(),
        vec![maho_core::skills::Skill {
            name: "guide".into(),
            description: "guide skill".into(),
            file_path: "/tmp/guide/SKILL.md".into(),
            base_dir: "/tmp/guide".into(),
            source_info: maho_core::source_info::create_synthetic_source_info(
                "/tmp/guide/SKILL.md",
                Default::default(),
            ),
            disable_model_invocation: false,
            provenance: None,
            allowed_set: None,
        }],
    );
    mode.handle_ui_request(
        maho_interactive::interactive_extension_ui::UiRequest::SettingChanged(
            "enableSkillCommands".into(),
            serde_json::json!(true),
        ),
    );
    mode.editor.editor.set_text("/skill:gu");
    mode.render(80);
    mode.handle_input_at("\t", 0);
    assert!(mode.editor.editor.is_showing_autocomplete());
    mode.handle_input_at("\t", 1);
    assert!(
        mode.editor.editor.get_text().starts_with("/skill:guide"),
        "{}",
        mode.editor.editor.get_text()
    );

    session
        .with_settings_manager_mut(|settings| {
            settings.set(
                maho_core::settings_manager::SettingsScope::Global,
                &[("enableSkillCommands".to_owned(), serde_json::json!(false))]
                    .into_iter()
                    .collect(),
            )
        })
        .expect("setting");
    mode.handle_ui_request(
        maho_interactive::interactive_extension_ui::UiRequest::SettingChanged(
            "enableSkillCommands".into(),
            serde_json::json!(false),
        ),
    );
    mode.editor.editor.set_text("/skill:gu");
    mode.render(80);
    mode.handle_input_at("\t", 2);
    mode.handle_input_at("\t", 3);
    assert!(
        !mode.editor.editor.get_text().starts_with("/skill:guide"),
        "{}",
        mode.editor.editor.get_text()
    );
}

#[test]
fn extension_select_counts_down_its_timeout_in_the_title_from_the_mode_tick() {
    use maho_tui::tui::Component;
    let (mut mode, _directory, _session) = native_mode_with_session_and_provider_retaining_session(
        None,
        |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
        "faux",
        "faux-1",
        false,
    );
    let (reply, mut answer) = tokio::sync::oneshot::channel();
    mode.handle_ui_request(
        maho_interactive::interactive_extension_ui::UiRequest::Select {
            title: "Pick".into(),
            options: vec!["a".into(), "b".into()],
            timeout_ms: Some(2_000),
            reply,
        },
    );
    assert!(mode.render(80).join("\n").contains("Pick (2s)"));
    mode.tick(1_000_000.0);
    assert_eq!(
        answer.try_recv().expect("expired selector answers"),
        None,
        "expiry cancels the selector"
    );
}

#[tokio::test]
async fn runtime_input_listener_consumes_model_shortcut_before_dispatch() {
    use maho_ext_api::{ExtensionUi, TerminalInputResult};
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let _unsubscribe = mode
        .extension_ui
        .on_terminal_input(std::sync::Arc::new(|_| {
            Some(TerminalInputResult {
                consume: Some(true),
                data: None,
            })
        }))
        .expect("listener");
    mode.handle_runtime_input("\x0c", 0).await.expect("input");
    assert!(!mode.render(80).join("\n").contains("Select Model"));
}

#[test]
fn skill_invocation_replay_separates_collapsed_skill_and_user_request() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let text = maho_core::skill_invocation::format_skill_invocation_prompt(
        &[maho_core::skill_invocation::SkillInvocationPromptSkill {
            name: "sample".into(),
            file_path: "/sample/SKILL.md".into(),
            base_dir: "/sample".into(),
            body: "private-body".into(),
        }],
        Some("user-request"),
    );
    let message = serde_json::from_value(
        serde_json::json!({"role":"user","content":[{"type":"text","text":text}],"timestamp":1}),
    )
    .expect("message");
    mode.add_history_message(&message);
    let lines = mode.render(80).join("\n");
    assert!(lines.contains("sample"));
    assert!(lines.contains("user-request"));
    assert!(!lines.contains("private-body"));
    mode.set_tools_expanded(true);
    assert!(mode.render(80).join("\n").contains("private-body"));
}

#[tokio::test]
async fn extension_question_cancel_releases_composer_and_preserves_unanswered_ids() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let request = maho_ext_api::QuestionRequest {
        request_id: "q".into(),
        questions: vec![maho_ext_api::Question {
            id: "item".into(),
            header: "Header".into(),
            question: "Choose".into(),
            options: vec![maho_ext_api::QuestionOption {
                label: "A".into(),
                description: None,
            }],
            multi_select: false,
        }],
        wait_for_answer: true,
        timeout_ms: 0,
    };
    let answer = ui.question(request, Default::default());
    mode.render(80);
    mode.handle_input_at("\x1b", 0);
    let response = answer.await.expect("question");
    assert_eq!(response.status, maho_ext_api::QuestionStatus::Cancelled);
    assert_eq!(response.unanswered, vec!["item"]);
    mode.handle_input_at("draft", 1);
    assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[tokio::test]
async fn non_wait_question_keeps_composer_until_answer_chord() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let request = maho_ext_api::QuestionRequest {
        request_id: "async-q".into(),
        questions: vec![maho_ext_api::Question {
            id: "item".into(),
            header: "Header".into(),
            question: "Choose".into(),
            options: vec![maho_ext_api::QuestionOption {
                label: "A".into(),
                description: None,
            }],
            multi_select: false,
        }],
        wait_for_answer: false,
        timeout_ms: 0,
    };
    let answer = ui.question(request, Default::default());
    mode.render(80);
    mode.handle_input_at("draft", 0);
    assert_eq!(mode.editor.editor.get_text(), "draft");
    mode.handle_input_at("\x1ba", 1);
    mode.handle_input_at("\x1b", 2);
    mode.handle_input_at("continued", 3);
    assert_eq!(mode.editor.editor.get_text(), "draftcontinued");
    mode.handle_input_at("\x1ba", 4);
    mode.handle_input_at("\r", 5);
    assert_eq!(
        answer.await.expect("question").status,
        maho_ext_api::QuestionStatus::Answered
    );
    assert_eq!(mode.editor.editor.get_text(), "draftcontinued");
}

#[tokio::test]
async fn empty_enter_opens_pending_question_and_submits_selection() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let answer = ui.question(retained_question("empty-enter", false), Default::default());
    println!("{}", mode.render(80).join("\n"));

    mode.handle_input_at("\r", 0);
    println!("{}", mode.render(80).join("\n"));
    mode.handle_input_at(" ", 1);
    mode.handle_input_at("\x1b[13;5u", 2);

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), answer)
        .await.expect("bounded answer").expect("answer");
    assert_eq!(response.status, maho_ext_api::QuestionStatus::Answered);
    assert_eq!(response.answers["item"].selected, ["A"]);
}

#[tokio::test]
async fn question_shortcuts_preserve_existing_composer_text() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.editor.editor.set_text("draft");
    let ui = mode.extension_ui.clone();
    let answer = ui.question(retained_question("existing-draft", false), Default::default());
    mode.render(80);

    mode.handle_input_at("1", 0);
    mode.handle_input_at("x", 1);
    assert_eq!(mode.editor.editor.get_text(), "draft1x");
    mode.handle_input_at("\r", 2);

    assert_eq!(mode.submit_editor().await.expect("chat submission"),
        Some(maho_core::agent_session::PromptDisposition::Started));
    mode.handle_input_at("\x1ba", 3);
    mode.handle_input_at("1", 4);
    mode.handle_input_at("\x1b[13;5u", 5);
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), answer)
        .await.expect("bounded answer").expect("answer");
    assert_eq!(response.status, maho_ext_api::QuestionStatus::Answered);
    assert_eq!(response.answers["item"].selected, ["A"]);
}

#[tokio::test]
async fn cycling_pending_questions_does_not_retarget_a_draft_reply() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let first = ui.question(retained_question("cycle-first", false), Default::default());
    let second = ui.question(retained_question("cycle-second", false), Default::default());
    mode.render(80);
    mode.handle_input_at("x", 0);

    mode.handle_input_at("\x1b[1;3B", 1);
    mode.handle_input_at("y", 2);
    mode.handle_input_at("\r", 3);
    mode.dispatch_submission().await.expect("comment submission");

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), first)
        .await.expect("bounded first answer").expect("first answer");
    assert_eq!(response.comment.as_deref(), Some("xy"));
    drop(second);
}

#[tokio::test]
async fn question_comment_accepts_batched_pasted_and_kitty_input() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    for (input, expected) in [
        ("batched reply", "batched reply"),
        ("\x1b[200~pasted reply\x1b[201~", "pasted reply"),
        ("\x1b[97u", "a"),
        ("\x1b[27;1;97~", "a"),
    ] {
        let (mut mode, _directory) = native_mode();
        let ui = mode.extension_ui.clone();
        let answer = ui.question(retained_question("comment", false), Default::default());
        mode.render(80);

        mode.handle_input_at(input, 0);
        mode.handle_input_at("\r", 1);
        assert_eq!(mode.dispatch_submission().await.expect("comment submission"),
            Some(maho_core::agent_session::PromptDisposition::Handled));

        let response = tokio::time::timeout(std::time::Duration::from_secs(5), answer)
            .await.expect("bounded answer").expect("answer");
        assert_eq!(response.status, maho_ext_api::QuestionStatus::CommentSubmitted);
        assert_eq!(response.comment.as_deref(), Some(expected));
    }
}

#[tokio::test]
async fn pending_question_does_not_capture_dialog_digits() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let answer = ui.question(retained_question("dialog-pending", false), Default::default());
    let input = ui.input("Value", None, Default::default());
    mode.render(80);

    mode.handle_input_at("1", 0);
    mode.handle_input_at("\r", 1);

    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(5), input)
        .await.expect("bounded input").as_deref(), Some("1"));
    drop(answer);
}

#[tokio::test]
async fn blocking_and_async_questions_resolve_in_arrival_order() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    for blocking_first in [true, false] {
        let (mut mode, _directory) = native_mode();
        let ui = mode.extension_ui.clone();
        let mut first_request = retained_question("mixed-first", blocking_first);
        first_request.questions[0].multi_select = false;
        let mut second_request = retained_question("mixed-second", !blocking_first);
        second_request.questions[0].multi_select = false;
        let first = ui.question(first_request, Default::default());
        let second = ui.question(second_request, Default::default());
        mode.render(80);

        mode.handle_input_at("1", 0);
        let first_response = tokio::time::timeout(std::time::Duration::from_secs(5), first)
            .await.expect("bounded first answer").expect("first answer");
        mode.render(80);
        mode.handle_input_at("2", 1);
        let second_response = tokio::time::timeout(std::time::Duration::from_secs(5), second)
            .await.expect("bounded second answer").expect("second answer");

        assert_eq!(first_response.answers["item"].selected, ["A"]);
        assert_eq!(second_response.answers["item"].selected, ["B"]);
    }
}

#[tokio::test]
async fn dropped_async_question_removes_widget_and_keeps_composer() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let request = maho_ext_api::QuestionRequest {
        request_id: "dropped-q".into(),
        questions: vec![maho_ext_api::Question {
            id: "item".into(),
            header: "Header".into(),
            question: "discard-this-question".into(),
            options: vec![maho_ext_api::QuestionOption {
                label: "A".into(),
                description: None,
            }],
            multi_select: false,
        }],
        wait_for_answer: false,
        timeout_ms: 0,
    };
    let answer = ui.question(request, Default::default());
    assert!(mode.render(80).join("\n").contains("discard-this-question"));
    drop(answer);
    assert!(!mode.render(80).join("\n").contains("discard-this-question"));
    mode.handle_input_at("draft", 0);
    assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[tokio::test]
async fn expanded_nonblocking_question_keeps_original_timeout() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let request = maho_ext_api::QuestionRequest {
        request_id: "deadline".into(),
        questions: vec![maho_ext_api::Question {
            id: "item".into(),
            header: "Header".into(),
            question: "Choose".into(),
            options: vec![maho_ext_api::QuestionOption {
                label: "A".into(),
                description: None,
            }],
            multi_select: false,
        }],
        wait_for_answer: false,
        timeout_ms: 1000,
    };
    let answer = ui.question(request, Default::default());
    mode.render(80);
    mode.handle_input_at("\x1ba", 0);
    mode.tick(2000.0);
    let response = answer.await.expect("deadline response");
    assert_eq!(response.status, maho_ext_api::QuestionStatus::TimedOut);
    assert_eq!(response.unanswered, ["item"]);
    mode.handle_input_at("draft", 2001);
    assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[tokio::test]
async fn successive_questions_do_not_replace_an_unanswered_request() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let make = |id: &str| maho_ext_api::QuestionRequest {
        request_id: id.into(),
        questions: vec![maho_ext_api::Question {
            id: "item".into(),
            header: id.into(),
            question: "Choose".into(),
            options: vec![maho_ext_api::QuestionOption {
                label: "A".into(),
                description: None,
            }],
            multi_select: false,
        }],
        wait_for_answer: true,
        timeout_ms: 0,
    };
    let first = ui.question(make("first"), Default::default());
    let second = ui.question(make("second"), Default::default());
    mode.render(80);
    mode.handle_input_at("1", 0);
    assert_eq!(
        first.await.expect("first response").status,
        maho_ext_api::QuestionStatus::Answered
    );
    mode.render(80);
    mode.handle_input_at("1", 1);
    assert_eq!(
        second.await.expect("second response").status,
        maho_ext_api::QuestionStatus::Answered
    );
}

#[tokio::test]
async fn answer_command_addresses_the_second_pending_question_and_settles_only_it() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let make = |id: &str| maho_ext_api::QuestionRequest {
        request_id: id.into(),
        questions: vec![maho_ext_api::Question {
            id: "item".into(),
            header: format!("Header {id}"),
            question: "Choose".into(),
            options: vec![maho_ext_api::QuestionOption {
                label: "A".into(),
                description: None,
            }],
            multi_select: false,
        }],
        wait_for_answer: false,
        timeout_ms: 0,
    };
    let first = ui.question(make("first"), Default::default());
    let second = ui.question(make("second"), Default::default());
    mode.render(80);
    assert!(
        mode.render(80).join("\n").contains("Header first"),
        "the first pending question is shown"
    );
    mode.submit("/answer 2", Default::default())
        .await
        .expect("answer command");
    assert!(
        mode.render(80).join("\n").contains("Header second"),
        "expanding the second question"
    );
    mode.handle_input_at("\x1b", 1);
    assert!(
        mode.render(80).join("\n").contains("Header second"),
        "Esc keeps the question pending"
    );
    mode.handle_input_at("\x1ba", 2);
    mode.handle_input_at("1", 3);
    assert_eq!(
        second.await.expect("second response").status,
        maho_ext_api::QuestionStatus::Answered
    );
    assert!(
        mode.render(80).join("\n").contains("Header first"),
        "the first question is still pending"
    );
    drop(first);
}

#[tokio::test]
async fn answer_skip_dismisses_the_shown_question_and_keeps_the_other_pending() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let make = |id: &str| maho_ext_api::QuestionRequest {
        request_id: id.into(),
        questions: vec![maho_ext_api::Question {
            id: "item".into(),
            header: format!("Header {id}"),
            question: "Choose".into(),
            options: vec![maho_ext_api::QuestionOption {
                label: "A".into(),
                description: None,
            }],
            multi_select: false,
        }],
        wait_for_answer: false,
        timeout_ms: 0,
    };
    let first = ui.question(make("first"), Default::default());
    let second = ui.question(make("second"), Default::default());
    mode.render(80);
    mode.submit("/answer 2", Default::default())
        .await
        .expect("answer command");
    mode.submit("/answer skip", Default::default())
        .await
        .expect("skip command");
    let response = second.await.expect("skipped response");
    assert_eq!(response.status, maho_ext_api::QuestionStatus::Cancelled);
    assert_eq!(response.unanswered, ["item"]);
    mode.render(80);
    assert!(
        mode.render(80).join("\n").contains("Header first"),
        "the other question stays pending"
    );
    drop(first);
}

#[tokio::test]
async fn answer_list_numbering_and_unknown_numbers_reject_cleanly() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let make = |id: &str| maho_ext_api::QuestionRequest {
        request_id: id.into(),
        questions: vec![maho_ext_api::Question {
            id: "item".into(),
            header: format!("Header {id}"),
            question: "Choose".into(),
            options: vec![maho_ext_api::QuestionOption {
                label: "A".into(),
                description: None,
            }],
            multi_select: false,
        }],
        wait_for_answer: false,
        timeout_ms: 0,
    };
    let first = ui.question(make("first"), Default::default());
    let second = ui.question(make("second"), Default::default());
    mode.render(80);
    mode.submit("/answer 9", Default::default())
        .await
        .expect("unknown index");
    assert!(
        mode.render(80)
            .join("\n")
            .contains("Choose a pending question number or /answer skip."),
        "unknown numbers are rejected"
    );
    assert!(
        mode.render(80).join("\n").contains("Header first"),
        "the shown question is unchanged"
    );
    drop(first);
    drop(second);
}

fn retained_question(id: &str, wait_for_answer: bool) -> maho_ext_api::QuestionRequest {
    maho_ext_api::QuestionRequest {
        request_id: id.into(),
        questions: vec![maho_ext_api::Question {
            id: "item".into(),
            header: id.into(),
            question: "Choose".into(),
            options: vec![
                maho_ext_api::QuestionOption {
                    label: "A".into(),
                    description: None,
                },
                maho_ext_api::QuestionOption {
                    label: "B".into(),
                    description: None,
                },
            ],
            multi_select: true,
        }],
        wait_for_answer,
        timeout_ms: 60_000,
    }
}

fn retained_options() -> maho_ext_api::QuestionOptions {
    maho_ext_api::QuestionOptions {
        initial_draft: Some(maho_ext_api::QuestionDraft {
            answers: Some(
                [(
                    "item".into(),
                    maho_ext_api::QuestionAnswer {
                        selected: vec!["A".into()],
                        text: None,
                    },
                )]
                .into(),
            ),
            comment: Some("retained-comment".into()),
        }),
        ..Default::default()
    }
}

/// The retained answer with no comment: a present comment settles the response as
/// comment-submitted (senpi `submitOutcome`), which the list-selection test does not assert.
fn retained_answer_options() -> maho_ext_api::QuestionOptions {
    maho_ext_api::QuestionOptions {
        initial_draft: Some(maho_ext_api::QuestionDraft {
            answers: Some(
                [(
                    "item".into(),
                    maho_ext_api::QuestionAnswer {
                        selected: vec!["A".into()],
                        text: None,
                    },
                )]
                .into(),
            ),
            comment: None,
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn blocking_question_seeds_the_producer_draft() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let answer = ui.question(
        retained_question("blocking-retained", true),
        retained_options(),
    );
    mode.render(80);
    mode.handle_input_at("\x1b[13;5u", 1);
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), answer)
        .await
        .expect("bounded answer")
        .expect("answer");
    assert_eq!(response.answers["item"].selected, ["A"]);
    assert_eq!(response.comment.as_deref(), Some("retained-comment"));
}

#[tokio::test]
async fn async_question_reexpansion_retains_progress_and_initial_comment() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let answer = ui.question(
        retained_question("async-retained", false),
        retained_options(),
    );
    mode.render(80);
    mode.submit("/answer", Default::default())
        .await
        .expect("expand");
    mode.handle_input_at("2", 1);
    mode.handle_input_at("\x1b", 2);
    mode.submit("/answer", Default::default())
        .await
        .expect("reexpand");
    mode.handle_input_at("\x1b[13;5u", 3);
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), answer)
        .await
        .expect("bounded answer")
        .expect("answer");
    assert_eq!(response.answers["item"].selected, ["A", "B"]);
    assert_eq!(response.comment.as_deref(), Some("retained-comment"));
}

#[tokio::test]
async fn answer_list_selection_expands_the_selected_request() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let first = ui.question(retained_question("first-list", false), retained_options());
    let second = ui.question(
        retained_question("second-list", false),
        retained_answer_options(),
    );
    mode.render(80);
    mode.submit("/answer", Default::default())
        .await
        .expect("list");
    mode.handle_input_at("\x1b[B", 1);
    mode.handle_input_at("\r", 2);
    mode.render(80);
    mode.handle_input_at("\x1b[13;5u", 3);
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), second)
        .await
        .expect("bounded selection")
        .expect("selected answer");
    assert_eq!(response.status, maho_ext_api::QuestionStatus::Answered);
    assert_eq!(response.answers["item"].selected, ["A"]);
    drop(first);
}

#[tokio::test(start_paused = true)]
async fn live_deadline_does_not_use_the_fixed_attachment_timeout() {
    use maho_ext_api::ExtensionUi;
    let theme = maho_interactive::theme::Theme::builtin(
        "dark",
        maho_interactive::theme::ColorMode::Truecolor,
    )
    .expect("theme");
    let (ui, mut requests) =
        maho_interactive::interactive_extension_ui::InteractiveExtensionUi::channel(
            maho_interactive::interactive_extension_ui::extension_theme(&theme),
        );
    let signal = maho_ext_api::AbortSignal::default();
    let answer = ui.question(
        retained_question("live", false),
        maho_ext_api::QuestionOptions {
            dialog: maho_ext_api::ExtensionUiDialogOptions {
                signal: Some(signal.clone()),
                timeout_ms: Some(1),
            },
            get_deadline_at_ms: Some(std::sync::Arc::new(|| 100_000)),
            ..Default::default()
        },
    );
    let mut answer = Box::pin(answer);
    let maho_interactive::interactive_extension_ui::UiRequest::Question { reply, .. } =
        requests.recv().await.expect("question request")
    else {
        panic!("question request");
    };
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(answer.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    tokio::time::advance(std::time::Duration::from_millis(2)).await;
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(answer.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    signal.abort();
    assert_eq!(
        answer.await.expect("abort").status,
        maho_ext_api::QuestionStatus::Cancelled
    );
    drop(reply);
}

#[tokio::test]
async fn collapsed_and_expanded_countdowns_read_the_live_capped_deadline() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let live = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
    let source = live.clone();
    let signal = maho_ext_api::AbortSignal::default();
    let answer = ui.question(
        retained_question("display-live", false),
        maho_ext_api::QuestionOptions {
            dialog: maho_ext_api::ExtensionUiDialogOptions {
                signal: Some(signal.clone()),
                timeout_ms: Some(1),
            },
            hard_deadline_at_ms: Some(1),
            get_deadline_at_ms: Some(std::sync::Arc::new(move || {
                source.load(std::sync::atomic::Ordering::Relaxed)
            })),
            ..retained_options()
        },
    );
    assert!(mode.render(80).join("\n").contains("00:00"));
    mode.submit("/answer", Default::default())
        .await
        .expect("expand");
    assert!(mode.render(80).join("\n").contains("00:00"));
    mode.handle_input_at("\x1b", 1);
    mode.render(80);
    signal.abort();
    assert_eq!(
        answer.await.expect("producer abort").status,
        maho_ext_api::QuestionStatus::Cancelled
    );
}

#[tokio::test]
async fn reattached_question_replaces_stale_draft_without_changing_number() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let old = ui.question(retained_question("reattach", false), Default::default());
    let other = ui.question(retained_question("other", false), Default::default());
    mode.render(80);
    drop(old);
    let replacement = ui.question(retained_question("reattach", false), retained_options());
    mode.render(80);
    mode.submit("/answer 1", Default::default())
        .await
        .expect("first position");
    mode.handle_input_at("\x1b[13;5u", 1);
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), replacement)
        .await
        .expect("bounded replacement")
        .expect("replacement");
    assert_eq!(response.answers["item"].selected, ["A"]);
    assert_eq!(response.comment.as_deref(), Some("retained-comment"));
    drop(other);
}

#[tokio::test]
async fn ask_user_tool_card_uses_the_question_renderer_not_a_raw_argument_dump() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.handle_event(&maho_agent::types::AgentEvent::ToolExecutionStart {
        parent_tool_call_id: None,
        tool_call_id: "call-ask".into(),
        tool_name: "ask_user_question".into(),
        args: serde_json::json!({"questions": [{"header": "Pick one"}], "waitForAnswer": true}),
    });
    let text = mode.render(80).join("\n");
    assert!(text.contains("[Pick one]"), "question headers: {text}");
    assert!(text.contains("wait for answer"), "wait mode: {text}");
    assert!(
        !text.contains("\"questions\""),
        "the raw argument dump is replaced: {text}"
    );
}

#[tokio::test]
async fn ask_user_tool_result_summarises_the_answer_details() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.handle_event(&maho_agent::types::AgentEvent::ToolExecutionStart {
        parent_tool_call_id: None,
        tool_call_id: "call-ask".into(),
        tool_name: "ask_user_question".into(),
        args: serde_json::json!({"questions": [{"header": "Pick one"}]}),
    });
    mode.handle_event(&maho_agent::types::AgentEvent::ToolExecutionEnd { parent_tool_call_id: None,
        tool_call_id: "call-ask".into(), tool_name: "ask_user_question".into(),
        result: serde_json::json!({"content": [{"type": "text", "text": "The user responded"}], "details": {"status": "answered", "answers": {"a": "x"}, "unanswered": ["b"]}}),
        is_error: false,
    });
    let text = mode.render(80).join("\n");
    assert!(
        text.contains("answered; 1 answered; 1 unanswered"),
        "result summary: {text}"
    );
    assert!(
        text.contains("The user responded"),
        "result content: {text}"
    );
}

#[tokio::test]
async fn native_tree_copy_keeps_selector_open_and_does_not_prompt() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("hi", Default::default())
        .await
        .expect("seed turn");
    mode.submit("/tree", Default::default())
        .await
        .expect("tree");
    let before = mode.render(80);
    mode.handle_runtime_input("\x18", 0)
        .await
        .expect("copy key");
    assert!(mode.submit_editor().await.expect("copy action").is_none());
    mode.handle_runtime_input("\x1b", 1)
        .await
        .expect("close tree");
    mode.handle_input_at("draft", 2);
    assert_eq!(mode.editor.editor.get_text(), "draft");
    assert!(!before.is_empty());
    assert!(mode.agent_idle);
}

#[tokio::test]
async fn fork_selector_uses_current_user_history_and_cancel_keeps_transcript() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("fork-source", Default::default())
        .await
        .expect("turn");
    mode.submit("/fork", Default::default())
        .await
        .expect("selector");
    assert!(mode.render(80).join("\n").contains("Fork from Message"));
    mode.handle_input_at("\x1b", 0);
    mode.handle_input_at("draft", 1);
    assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[tokio::test]
async fn fork_selection_reopens_user_text_before_selected_message() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("fork-source", Default::default())
        .await
        .expect("turn");
    mode.submit("/fork", Default::default())
        .await
        .expect("selector");
    mode.render(80);
    mode.handle_input_at("\r", 0);
    mode.submit_editor().await.expect("fork");
    assert_eq!(mode.editor.editor.get_text(), "fork-source");
    assert_eq!(mode.footer_snapshot().context_tokens, Some(0.0));
}

#[tokio::test]
async fn clone_keeps_current_leaf_transcript_and_clears_composer() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("clone-source", Default::default())
        .await
        .expect("turn");
    mode.editor.editor.set_text("draft");
    mode.submit("/clone", Default::default())
        .await
        .expect("clone");
    assert!(mode.editor.editor.get_text().is_empty());
    assert!(mode.render(80).join("\n").contains("clone-source"));
}

#[tokio::test]
async fn native_tree_selector_cancel_retains_session_and_releases_composer() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("tree-source", Default::default())
        .await
        .expect("turn");
    mode.submit("/tree", Default::default())
        .await
        .expect("tree");
    mode.render(80);
    mode.handle_input_at("\x1b", 0);
    mode.handle_input_at("draft", 1);
    assert_eq!(mode.editor.editor.get_text(), "draft");
    assert!(mode.render(80).join("\n").contains("tree-source"));
}

#[tokio::test]
async fn fork_selector_defaults_to_latest_user_message() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("earlier-user", Default::default())
        .await
        .expect("first");
    mode.submit("latest-user", Default::default())
        .await
        .expect("second");
    mode.submit("/fork", Default::default())
        .await
        .expect("selector");
    mode.render(80);
    mode.handle_input_at("\r", 0);
    mode.submit_editor().await.expect("fork");
    assert_eq!(mode.editor.editor.get_text(), "latest-user");
    assert!(mode.render(80).join("\n").contains("earlier-user"));
}

#[tokio::test]
async fn model_management_commands_cancel_without_consuming_provider_turn() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    for command in ["/scoped-models", "/favorite-models"] {
        assert_eq!(
            mode.submit(command, Default::default())
                .await
                .expect("selector"),
            maho_core::agent_session::PromptDisposition::Handled
        );
        for width in [40, 80, 120] {
            assert!(mode.render(width).join("\n").contains("faux-1"));
        }
        mode.handle_input_at("\x1b", 0);
        mode.handle_input_at("draft", 1);
        assert_eq!(mode.editor.editor.get_text(), "draft");
        mode.editor.editor.set_text("");
    }
    mode.submit("hi", Default::default()).await.expect("turn");
    assert!(mode.render(80).join("\n").contains("hello"));
}

#[tokio::test]
async fn scoped_models_selector_refreshes_catalogs_while_open() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("/scoped-models", Default::default())
        .await
        .expect("selector");
    assert!(
        mode.render(80)
            .join("\n")
            .contains("Refreshing model catalogs…")
    );
    let mut refreshed = false;
    for _ in 0..100_000 {
        if mode
            .render(80)
            .join("\n")
            .contains("Model catalogs refreshed.")
        {
            refreshed = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        refreshed,
        "the refresh outcome replaces the refreshing line"
    );
    mode.handle_input_at("\x1b", 0);
}

#[tokio::test]
async fn scoped_model_selection_persists_and_restores_empty_enabled_set() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("/scoped-models", Default::default())
        .await
        .expect("selector");
    mode.render(80);
    mode.handle_input_at("\x18", 0);
    mode.handle_input_at("\x13", 1);
    mode.handle_input_at("\x1b", 2);
    mode.render(80);
    mode.submit("/scoped-models", Default::default())
        .await
        .expect("reopen");
    let rendered = mode.render(80).join("\n");
    assert!(!rendered.contains('✓'));
    mode.handle_input_at("\x1b", 3);
    mode.handle_input_at("draft", 4);
    assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[tokio::test]
async fn configured_session_shortcut_opens_native_selector() {
    use maho_tui::tui::Component;
    let (mut mode, directory) = native_mode();
    std::fs::write(
        directory.path().join("keybindings.json"),
        r#"{"app.session.renameCurrent":"ctrl+r"}"#,
    )
    .expect("configuration");
    mode.handle_input_at("\x12", 0);
    mode.render(80);
    mode.handle_input_at("configured-name", 1);
    mode.handle_input_at("\r", 2);
    assert_eq!(
        mode.footer_snapshot().session_name.as_deref(),
        Some("configured-name")
    );
}

#[tokio::test]
async fn trust_command_saves_only_future_project_decision() {
    use maho_tui::tui::Component;
    let (mut mode, directory) = native_mode();
    mode.submit("/trust", Default::default())
        .await
        .expect("trust selector");
    mode.render(80);
    mode.handle_input_at("\r", 0);
    let store =
        maho_core::trust_manager::ProjectTrustStore::new(&directory.path().to_string_lossy());
    assert_eq!(
        store
            .get(&directory.path().to_string_lossy())
            .expect("saved trust"),
        Some(true)
    );
    mode.handle_input_at("draft", 1);
    assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[tokio::test]
async fn settings_menu_toggles_native_compaction_and_releases_composer() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("/settings", Default::default())
        .await
        .expect("settings");
    for width in [40, 80, 120] {
        mode.render(width);
    }
    mode.handle_input_at("\r", 0);
    mode.handle_input_at("\x1b", 1);
    mode.submit("/settings", Default::default())
        .await
        .expect("reopen");
    let rendered = mode.render(120).join("\n");
    assert!(
        rendered
            .lines()
            .any(|line| line.contains("Auto-compact") && line.contains("false"))
    );
    mode.handle_input_at("\x1b", 2);
    mode.handle_input_at("draft", 3);
    assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[tokio::test]
async fn resume_menu_cancellation_keeps_current_native_transcript() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("resume-source", Default::default())
        .await
        .expect("turn");
    assert_eq!(
        mode.submit("/resume", Default::default())
            .await
            .expect("resume menu"),
        maho_core::agent_session::PromptDisposition::Handled
    );
    for width in [40, 80, 120] {
        mode.render(width);
    }
    mode.handle_input_at("\x1b", 0);
    mode.handle_input_at("draft", 1);
    assert_eq!(mode.editor.editor.get_text(), "draft");
    assert!(mode.render(80).join("\n").contains("resume-source"));
}

#[tokio::test]
async fn image_payloads_follow_submitted_marker_order() {
    use maho_tui::tui::Component;
    let (mut mode, directory) = native_mode();
    mode.attach_image(maho_ai::types::ImageContent {
        data: "first".into(),
        mime_type: "image/png".into(),
    });
    mode.handle_input_at("\x1b[H", 0);
    mode.attach_image(maho_ai::types::ImageContent {
        data: "second".into(),
        mime_type: "image/png".into(),
    });
    mode.handle_input("\r");
    mode.submit_editor().await.expect("image prompt");
    let path = directory.path().join("images.jsonl");
    mode.submit(&format!("/export {}", path.display()), Default::default())
        .await
        .expect("export");
    let entries: Vec<serde_json::Value> = std::fs::read_to_string(path)
        .expect("exported session")
        .lines()
        .map(|line| serde_json::from_str(line).expect("entry"))
        .collect();
    let user = entries
        .iter()
        .find(|entry| entry["message"]["role"] == "user")
        .expect("user message");
    let images: Vec<_> = user["message"]["content"]
        .as_array()
        .expect("content")
        .iter()
        .filter(|part| part["type"] == "image")
        .map(|part| part["data"].as_str().expect("image data"))
        .collect();
    assert_eq!(images, ["second", "first"]);
}

struct ScreenTerminal {
    writes: String,
    screen: maho_test_support::vterm::VirtualTerminal,
    input: Option<maho_tui::terminal::InputHandler>,
    stopped: bool,
}

#[tokio::test]
async fn faux_screen_hi_matches_pinned_interactive_cells() {
    for (width, fixture) in [
        (40, "senpi-hi-40.cells.json"),
        (80, "senpi-hi-80.cells.json"),
        (120, "senpi-hi.cells.json"),
    ] {
        let (mut mode, _directory) = native_mode_at(Some("/tmp"));
        mode.handle_input_at("hi", 0);
        mode.handle_input_at("\r", 1);
        mode.submit_editor().await.expect("native hi");
        let theme = maho_interactive::theme::Theme::builtin(
            "dark",
            maho_interactive::theme::ColorMode::Truecolor,
        )
        .expect("theme");
        let renderer = maho_interactive::tui_renderer::create_interactive_tui(
            maho_interactive::tui_renderer::InteractiveTuiOptions {
                tui_mode: maho_interactive::tui_renderer::TuiMode::Fullscreen,
                show_hardware_cursor: false,
                bottom_shortcut: String::new(),
            },
            theme,
        );
        let mut mounted =
            maho_interactive::interactive_terminal::InteractiveTerminal::new(mode, renderer);
        let mut terminal = ScreenTerminal {
            writes: String::new(),
            screen: maho_test_support::vterm::VirtualTerminal::new(width, 36),
            input: None,
            stopped: false,
        };
        mounted.start(&mut terminal, false, false);
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.omo/evidence/task-35-faux")
            .join(fixture);
        let expected: maho_test_support::vterm::Screen =
            serde_json::from_str(&std::fs::read_to_string(fixture).expect("pinned fixture"))
                .expect("pinned cells");
        let actual = terminal.screen.snapshot();
        println!(
            "NATIVE_HI_ANSI_{width}={}",
            serde_json::to_string(&terminal.writes).expect("ANSI evidence")
        );
        mounted.stop(&mut terminal, true).expect("restore terminal");
        assert_footer_divergent_screen_matches(&actual, &expected);
    }
}
/// Every row matches the pinned senpi screen except the last, the footer, which diverges by
/// design (docs/divergences.md UX-2) and is checked for the v7 layout instead.
fn assert_footer_divergent_screen_matches(
    actual: &maho_test_support::vterm::Screen,
    expected: &maho_test_support::vterm::Screen,
) {
    let footer = actual.cells.len() - 1;
    assert_eq!(
        actual.cells[..footer], expected.cells[..footer],
        "native viewport: {:?}",
        actual.viewport
    );
    let row = actual.viewport[footer].trim_end();
    assert!(
        row.starts_with("/tmp ") && row.contains("ctx 0% 4/128K") && row.ends_with("faux-1"),
        "v7 footer row: {row:?}"
    );
}
#[tokio::test]
async fn faux_screen_slash_hotkeys_matches_pinned_cells() {
    let (mut mode, _directory) = native_mode_at(Some("/tmp"));
    mode.handle_input_at("hi", 0);
    mode.handle_input_at("\r", 1);
    mode.submit_editor().await.expect("seed turn");
    mode.handle_input_at("/hotkeys", 2);
    mode.handle_input_at("\r", 3);
    mode.submit_editor().await.expect("hotkeys");
    let theme = maho_interactive::theme::Theme::builtin(
        "dark",
        maho_interactive::theme::ColorMode::Truecolor,
    )
    .expect("theme");
    let renderer = maho_interactive::tui_renderer::create_interactive_tui(
        maho_interactive::tui_renderer::InteractiveTuiOptions {
            tui_mode: maho_interactive::tui_renderer::TuiMode::Fullscreen,
            show_hardware_cursor: false,
            bottom_shortcut: String::new(),
        },
        theme,
    );
    let mut mounted =
        maho_interactive::interactive_terminal::InteractiveTerminal::new(mode, renderer);
    let mut terminal = ScreenTerminal {
        writes: String::new(),
        screen: maho_test_support::vterm::VirtualTerminal::new(120, 36),
        input: None,
        stopped: false,
    };
    mounted.start(&mut terminal, false, false);
    let fixture = if cfg!(target_os = "macos") {
        include_str!("../../../.omo/evidence/task-35-slash/senpi-hotkeys-120-darwin.cells.json")
    } else if cfg!(windows) {
        include_str!("golden/senpi-hotkeys-120-windows.cells.json")
    } else {
        include_str!("../../../.omo/evidence/task-35-slash/senpi-hotkeys-120.cells.json")
    };
    let expected: maho_test_support::vterm::Screen =
        serde_json::from_str(fixture).expect("pinned cells");
    let actual = terminal.screen.snapshot();
    println!(
        "NATIVE_HOTKEYS_ANSI={}",
        serde_json::to_string(&terminal.writes).expect("ANSI evidence")
    );
    mounted.stop(&mut terminal, true).expect("restore terminal");
    assert_footer_divergent_screen_matches(&actual, &expected);
}

impl maho_tui::terminal::Terminal for ScreenTerminal {
    fn start(
        &mut self,
        input: maho_tui::terminal::InputHandler,
        _: maho_tui::terminal::ResizeHandler,
    ) {
        self.input = Some(input);
        self.screen.start();
    }
    fn stop(&mut self) -> Result<(), maho_tui::terminal::TerminalError> {
        self.input = None;
        self.stopped = true;
        self.screen.stop();
        Ok(())
    }
    fn drain_input(&mut self, _: u64, _: u64) {}
    fn write(&mut self, data: &str) {
        self.writes.push_str(data);
        self.screen.write(data);
    }
    fn columns(&self) -> u16 {
        self.screen.columns()
    }
    fn rows(&self) -> u16 {
        self.screen.rows()
    }
    fn kitty_protocol_active(&self) -> bool {
        true
    }
    fn move_by(&mut self, lines: i64) {
        self.screen.move_by(i32::try_from(lines).expect("lines"));
    }
    fn hide_cursor(&mut self) {
        self.screen.hide_cursor();
    }
    fn show_cursor(&mut self) {
        self.screen.show_cursor();
    }
    fn clear_line(&mut self) {
        self.screen.clear_line();
    }
    fn clear_from_cursor(&mut self) {
        self.screen.clear_from_cursor();
    }
    fn clear_screen(&mut self) {
        self.screen.clear_screen();
    }
    fn set_title(&mut self, title: &str) {
        self.screen.set_title(title);
    }
    fn set_progress(&mut self, _: bool) {}
}

#[tokio::test]
async fn mounted_terminal_renders_native_turn_and_restores_terminal_on_stop() {
    for width in [40, 80, 120] {
        let (mut mode, _directory) = native_mode();
        mode.submit("hi", Default::default()).await.expect("turn");
        let theme = maho_interactive::theme::Theme::builtin(
            "dark",
            maho_interactive::theme::ColorMode::Truecolor,
        )
        .expect("theme");
        let renderer = maho_interactive::tui_renderer::create_interactive_tui(
            maho_interactive::tui_renderer::InteractiveTuiOptions {
                tui_mode: maho_interactive::tui_renderer::TuiMode::Fullscreen,
                show_hardware_cursor: false,
                bottom_shortcut: String::new(),
            },
            theme,
        );
        let mut mounted =
            maho_interactive::interactive_terminal::InteractiveTerminal::new(mode, renderer);
        let mut terminal = ScreenTerminal {
            writes: String::new(),
            screen: maho_test_support::vterm::VirtualTerminal::new(width, 36),
            input: None,
            stopped: false,
        };
        mounted.start(&mut terminal, false, false);
        let dimensions = std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured = dimensions.clone();
        {
            use maho_ext_api::ExtensionUi;
            mounted
                .mode
                .borrow()
                .extension_ui
                .set_header_factory(Some(std::sync::Arc::new(move |host, _| {
                    *captured.lock().expect("dimensions") = Some(host.dimensions());
                    Box::new(maho_tui::components::text::Text::new("header"))
                })))
                .expect("header factory");
        }
        mounted.render(&mut terminal);
        assert_eq!(*dimensions.lock().expect("dimensions"), Some((width, 36)));
        terminal.input.as_mut().expect("input")("draft");
        while let Some(input) = mounted.take_input(&mut terminal, 0) {
            mounted.mode.borrow_mut().handle_input_at(&input, 0);
        }
        assert_eq!(mounted.mode.borrow().editor.editor.get_text(), "draft");
        mounted.render(&mut terminal);
        assert!(terminal.screen.viewport().join("\n").contains("hello"));
        assert!(terminal.screen.bracketed_paste());
        mounted.stop(&mut terminal, true).expect("stop");
        assert!(terminal.stopped);
        assert!(!terminal.screen.bracketed_paste());
        assert!(!terminal.screen.cursor_hidden());
    }
}

#[tokio::test]
async fn extension_actions_expose_theme_catalog_and_live_working_indicator() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory, session) = native_mode_with_session_and_provider_retaining_session(
        None,
        |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
        "faux",
        "faux-1",
        false,
    );
    let ui = mode.extension_ui.clone();
    assert!(
        ui.get_all_themes()
            .expect("themes")
            .iter()
            .any(|theme| theme.name == "light")
    );
    assert!(
        ui.set_theme(maho_ext_api::ThemeSelection::Name("light".into()))
            .expect("theme result")
            .success
    );
    mode.render(80);
    assert_eq!(ui.theme().name.as_deref(), Some("light"));
    assert_eq!(
        session
            .with_settings_manager(|settings| settings.get_string("theme"))
            .as_deref(),
        Some("light"),
        "an extension theme switch is saved as the global setting"
    );
    ui.set_working_indicator(Some(maho_ext_api::WorkingIndicatorOptions {
        frames: Some(vec!["frame-a".into(), "frame-b".into()]),
        interval_ms: Some(10),
    }))
    .expect("indicator");
    mode.render(80);
    mode.handle_event(&maho_agent::types::AgentEvent::AgentStart);
    assert!(
        mode.working_frame(0.0)
            .is_some_and(|frame| frame == "frame-a")
    );
}

struct CustomChoice {
    done: maho_ext_api::CustomUiDone,
}
impl maho_tui::tui::Component for CustomChoice {
    fn render(&mut self, _: usize) -> Vec<String> {
        vec!["custom-choice".into()]
    }
    fn handle_input(&mut self, _: &str) {
        (self.done)(serde_json::json!({"selected":7}));
    }
    fn has_input_handler(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn custom_factory_completion_restores_editor_and_returns_value() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let factory: maho_ext_api::CustomComponentFactory = std::sync::Arc::new(|_, _, _, done| {
        Box::pin(async move { Ok(Box::new(CustomChoice { done }) as Box<dyn Component>) })
    });
    let result = ui.custom_factory(factory, Default::default());
    assert!(mode.render(80).join("\n").contains("custom-choice"));
    mode.handle_input_at("select", 0);
    mode.render(80);
    assert_eq!(
        result.await.expect("completed factory"),
        serde_json::json!({"selected":7})
    );
    mode.handle_input_at("draft", 1);
    assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[tokio::test]
async fn debug_command_writes_the_rendered_report_to_the_configured_path() {
    use maho_tui::tui::Component;
    let (mut mode, directory) = native_mode();
    let path = directory.path().join("debug.log");
    mode.set_debug_log_path(Some(path.to_string_lossy().into_owned()));
    mode.submit("/debug", Default::default())
        .await
        .expect("debug command");
    let data = std::fs::read_to_string(&path).expect("debug log");
    assert!(data.contains("Terminal: "), "terminal size line");
    assert!(
        data.contains("=== All rendered lines with visible widths ==="),
        "render dump header"
    );
    assert!(
        data.contains("=== Agent messages (JSONL) ==="),
        "message dump header"
    );
    assert!(
        mode.render(80).join("\n").contains("Debug log written"),
        "status feedback"
    );
}

#[tokio::test]
async fn import_command_confirms_copies_and_switches_to_the_jsonl() {
    use maho_tui::tui::Component;
    let (mut mode, directory) = native_mode_with_session(None, |cwd| {
        let session_dir = std::path::Path::new(cwd).join("sessions");
        std::fs::create_dir_all(&session_dir).expect("session dir");
        let session_dir = session_dir.to_string_lossy().into_owned();
        maho_core::session_manager::SessionManager::create(cwd, Some(&session_dir), None)
    });
    let source_dir = tempfile::tempdir().expect("source directory");
    let source = source_dir.path().join("imported.jsonl");
    let cwd = directory.path().to_string_lossy().into_owned();
    let header = serde_json::json!({"type":"session","version":3,"id":"imported","timestamp":"2026-10-03T00:00:00.000Z","cwd":cwd});
    let message = serde_json::json!({"type":"message","id":"m1","parentId":null,"timestamp":"2026-10-03T00:00:01.000Z","message":{"role":"user","content":"imported prompt","timestamp":1790985601000i64}});
    std::fs::write(&source, format!("{header}\n{message}\n")).expect("fixture");
    mode.submit(&format!("/import {}", source.display()), Default::default())
        .await
        .expect("confirm dialog");
    assert!(
        mode.render(80)
            .join("\n")
            .contains("Replace current session with"),
        "confirmation prompt"
    );
    mode.handle_input_at("\r", 0);
    mode.submit_editor().await.expect("import command");
    let text = mode.render(80).join("\n");
    assert!(
        text.contains("imported prompt"),
        "imported transcript: {text}"
    );
    assert!(text.contains("Session imported from:"), "import status");
}

#[tokio::test]
async fn import_command_reports_a_missing_file_before_any_session_change() {
    let (mut mode, directory) = native_mode();
    let missing = directory.path().join("absent.jsonl");
    assert_eq!(
        mode.submit(
            &format!("/import {}", missing.display()),
            Default::default()
        )
        .await
        .expect("confirm dialog"),
        maho_core::agent_session::PromptDisposition::Handled
    );
    mode.handle_input_at("\r", 0);
    let error = mode.submit_editor().await.expect_err("missing import file");
    assert_eq!(
        error,
        format!(
            "Failed to import session: File not found: {}",
            missing.display()
        )
    );
}

#[tokio::test]
async fn import_without_arguments_reports_usage() {
    let (mut mode, _directory) = native_mode();
    assert_eq!(
        mode.submit("/import", Default::default())
            .await
            .expect_err("usage"),
        "Usage: /import <path.jsonl>"
    );
}

#[tokio::test]
async fn startup_changelog_notices_show_only_new_entries_and_record_the_seen_version() {
    use maho_tui::tui::Component;
    let (mut mode, directory) = native_mode();
    let path = directory.path().join("CHANGELOG.md");
    std::fs::write(
        &path,
        "## [0.82.0] - 2026-01-03\nNewest entry\n\n## [0.81.0] - 2026-01-02\nOlder entry\n",
    )
    .expect("changelog");
    let source = |version: &str| maho_core::changelog_source::ChangelogSource {
        id: "engine".into(),
        path: path.to_string_lossy().into_owned(),
        version: Some(version.into()),
        rewrite_links: false,
    };
    // A fresh install records the version without showing anything.
    mode.load_startup_changelog_from(&source("0.81.0"));
    assert!(
        mode.changelog_markdown().is_none(),
        "a fresh install shows no changelog"
    );
    assert_eq!(
        mode.changelog_seen("engine").as_deref(),
        Some("0.81.0"),
        "the first-seen version is recorded"
    );
    // A later build shows only the entries newer than the recorded version.
    mode.load_startup_changelog_from(&source("0.82.0"));
    assert!(
        mode.changelog_markdown()
            .is_some_and(|markdown| markdown.contains("Newest entry")),
        "the newer entry is pending"
    );
    mode.show_startup_notices_if_needed();
    let text = mode.render(80).join("\n");
    assert!(text.contains("What's New"), "heading: {text}");
    assert!(text.contains("Newest entry"), "new entry body: {text}");
    assert!(
        !text.contains("Older entry"),
        "the already-seen entry is hidden: {text}"
    );
    assert_eq!(
        mode.changelog_seen("engine").as_deref(),
        Some("0.82.0"),
        "the newer version is recorded"
    );
    // The notices render once.
    mode.show_startup_notices_if_needed();
    assert_eq!(
        mode.render(80).join("\n").matches("What's New").count(),
        1,
        "startup notices render only once"
    );
}

#[tokio::test]
async fn tree_edit_prefills_assistant_text_and_rejects_other_entries() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode_with_session(None, |cwd| {
        let entries = vec![
            serde_json::json!({"type":"session","version":3,"id":"tree-edit","timestamp":"2026-10-03T00:00:00.000Z","cwd":cwd}),
            serde_json::json!({"type":"message","id":"u1","parentId":null,"timestamp":"2026-10-03T00:00:00.000Z","message":{"role":"user","content":"hello"}}),
            serde_json::json!({"type":"message","id":"a1","parentId":"u1","timestamp":"2026-10-03T00:00:01.000Z","message":{"role":"assistant","content":[{"type":"text","text":"assistant reply"}],"api":"faux","provider":"faux","model":"faux-1","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},"stopReason":"stop","timestamp":0}}),
        ];
        maho_core::session_manager::SessionManager::in_memory(cwd, None, Some(entries))
    });
    // senpi `editAssistantMessageFromTree`: a non-assistant entry is refused.
    mode.submit(
        &maho_interactive::interactive_mode::internal_command("/tree-edit u1"),
        Default::default(),
    )
    .await
    .expect("command");
    assert!(
        mode.render(80)
            .join("\n")
            .contains("Only assistant responses can be edited here"),
        "non-assistant entries are rejected"
    );
    // An assistant entry opens the editor prefilled with its text.
    mode.submit(
        &maho_interactive::interactive_mode::internal_command("/tree-edit a1"),
        Default::default(),
    )
    .await
    .expect("command");
    assert!(
        mode.render(80).join("\n").contains("assistant reply"),
        "the editor is prefilled with the assistant text"
    );
}

#[tokio::test]
async fn tree_edit_apply_without_a_pending_edit_is_a_noop() {
    let (mut mode, _directory) = native_mode();
    mode.submit(
        &maho_interactive::interactive_mode::internal_command("/tree-edit-apply"),
        Default::default(),
    )
    .await
    .expect("no-op");
}

#[tokio::test]
async fn tree_navigate_without_a_pending_choice_is_a_noop() {
    let (mut mode, _directory) = native_mode();
    mode.submit(
        &maho_interactive::interactive_mode::internal_command("/tree-navigate"),
        Default::default(),
    )
    .await
    .expect("no-op");
}

#[tokio::test]
async fn tree_navigation_prompts_for_a_branch_summary_unless_skipped() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode_with_session(None, |cwd| {
        let entries = vec![
            serde_json::json!({"type":"session","version":3,"id":"tree-nav","timestamp":"2026-10-03T00:00:00.000Z","cwd":cwd}),
            serde_json::json!({"type":"message","id":"u1","parentId":null,"timestamp":"2026-10-03T00:00:00.000Z","message":{"role":"user","content":"hello"}}),
            serde_json::json!({"type":"message","id":"a1","parentId":"u1","timestamp":"2026-10-03T00:00:01.000Z","message":{"role":"assistant","content":[{"type":"text","text":"reply"}],"api":"faux","provider":"faux","model":"faux-1","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},"stopReason":"stop","timestamp":0}}),
        ];
        maho_core::session_manager::SessionManager::in_memory(cwd, None, Some(entries))
    });
    // senpi `runTreeNavigation(entryId, { promptForSummary: true })`: the summary choice is asked.
    mode.submit(
        &maho_interactive::interactive_mode::internal_command("/tree u1"),
        Default::default(),
    )
    .await
    .expect("command");
    let text = mode.render(80).join("\n");
    assert!(
        text.contains("Summarize branch?"),
        "the branch summary selector mounts: {text}"
    );
    assert!(
        text.contains("Summarize with custom prompt"),
        "the custom option is offered: {text}"
    );
}

#[tokio::test]
async fn startup_changelog_records_without_showing_on_a_fresh_install() {
    use maho_tui::tui::Component;
    let (mut mode, directory) = native_mode();
    let path = directory.path().join("CHANGELOG.md");
    std::fs::write(&path, "## 0.82.0 - 2026-01-03\nNewest entry\n").expect("changelog");
    let source = maho_core::changelog_source::ChangelogSource {
        id: "engine".into(),
        path: path.to_string_lossy().into_owned(),
        version: Some("0.82.0".into()),
        rewrite_links: false,
    };
    mode.load_startup_changelog_from(&source);
    mode.show_startup_notices_if_needed();
    // With no recorded version the install is fresh: record it, show nothing.
    assert_eq!(mode.changelog_seen("engine").as_deref(), Some("0.82.0"));
    assert!(
        !mode.render(80).join("\n").contains("What's New"),
        "a fresh install shows no notices"
    );
}

#[tokio::test]
async fn startup_header_mounts_the_welcome_content_with_a_tip_and_follows_the_toggle() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.mount_startup_header_at(0);
    let collapsed = mode.render(80).join("\n");
    let app = maho_core::config::app_command();
    assert!(!app.is_empty());
    assert!(
        collapsed.contains(app.as_str()),
        "the welcome logo names the app: {collapsed}"
    );
    assert!(
        collapsed.contains("can explain its own features"),
        "onboarding: {collapsed}"
    );
    assert!(
        collapsed.contains("to show full startup help and loaded resources"),
        "collapsed help: {collapsed}"
    );
    assert!(
        collapsed.contains("Tip:"),
        "the tip sibling renders: {collapsed}"
    );
    assert!(
        !mode.tips_history().is_empty(),
        "the shown tip is recorded in tipsHistory"
    );
    assert!(
        !collapsed.contains("to interrupt"),
        "the expanded instructions stay hidden: {collapsed}"
    );
    mode.set_tools_expanded(true);
    let expanded = mode.render(80).join("\n");
    assert!(
        expanded.contains("to interrupt"),
        "the expansion toggle swaps in the full instructions: {expanded}"
    );
    assert!(
        !expanded.contains("to show full startup help and loaded resources"),
        "the collapsed hint is replaced: {expanded}"
    );
}

#[tokio::test]
async fn extension_header_replaces_the_built_in_header_and_restores_it() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.mount_startup_header_at(0);
    assert!(
        mode.render(80)
            .join("\n")
            .contains("can explain its own features"),
        "the built-in header mounts first"
    );
    let ui = mode.extension_ui.clone();
    ui.set_header_factory(Some(std::sync::Arc::new(|_, _| {
        Box::new(maho_tui::components::text::Text::new("CUSTOM HEADER"))
    })))
    .expect("header factory");
    let with_custom = mode.render(80).join("\n");
    assert!(
        with_custom.contains("CUSTOM HEADER"),
        "the extension header renders: {with_custom}"
    );
    assert!(
        !with_custom.contains("can explain its own features"),
        "the built-in header is replaced: {with_custom}"
    );
    ui.set_header_factory(None).expect("header factory cleared");
    assert!(
        mode.render(80)
            .join("\n")
            .contains("can explain its own features"),
        "the built-in header is restored"
    );
}

#[tokio::test]
async fn replacement_commands_route_through_the_mounted_session_host() {
    use maho_interactive::interactive_session::{
        ForkOutcome, InteractiveSession, ReplacementOutcome, SessionFuture,
    };
    use std::sync::{Arc, Mutex};

    struct SpyHost {
        calls: Arc<Mutex<Vec<String>>>,
    }
    impl InteractiveSession for SpyHost {
        fn session_id(&self) -> Option<String> {
            None
        }
        fn session_file(&self) -> Option<String> {
            None
        }
        fn cwd(&self) -> String {
            String::new()
        }
        fn is_reconnecting(&self) -> bool {
            false
        }
        fn is_fallback(&self) -> bool {
            false
        }
        fn remote_state(
            &self,
        ) -> Option<maho_interactive::interactive_host_runtime::RemoteSessionState> {
            None
        }
        fn subscribe_session_events(
            &self,
            _listener: maho_interactive::interactive_host_runtime::SessionEventListener,
        ) -> Option<maho_interactive::interactive_host_runtime::SessionSubscription> {
            None
        }
        fn request_remote_history(
            &self,
            _generation: u64,
            _sender: tokio::sync::mpsc::UnboundedSender<(
                u64,
                Result<Vec<serde_json::Value>, String>,
            )>,
        ) {
        }
        fn new_session(
            &self,
            _parent_session: Option<String>,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            let calls = self.calls.clone();
            Box::pin(async move {
                calls.lock().expect("calls").push("new_session".into());
                Ok(ReplacementOutcome::Replaced)
            })
        }
        fn switch_session(
            &self,
            path: String,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            let calls = self.calls.clone();
            Box::pin(async move {
                calls
                    .lock()
                    .expect("calls")
                    .push(format!("switch_session:{path}"));
                Ok(ReplacementOutcome::Replaced)
            })
        }
        fn fork(
            &self,
            entry_id: String,
            include_entry: bool,
        ) -> SessionFuture<'_, Result<ForkOutcome, String>> {
            let calls = self.calls.clone();
            Box::pin(async move {
                calls
                    .lock()
                    .expect("calls")
                    .push(format!("fork:{entry_id}:{include_entry}"));
                Ok(ForkOutcome {
                    outcome: ReplacementOutcome::Replaced,
                    editor_text: Some("selected".into()),
                })
            })
        }
        fn export_html(
            &self,
            output_path: Option<String>,
            theme_name: Option<String>,
        ) -> SessionFuture<'_, Result<Option<String>, String>> {
            let calls = self.calls.clone();
            Box::pin(async move {
                calls.lock().expect("calls").push(format!(
                    "export_html:{}:{}",
                    output_path.as_deref().unwrap_or(""),
                    theme_name.as_deref().unwrap_or("")
                ));
                Ok(output_path)
            })
        }
        fn dispose(&self) -> SessionFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    let (mut mode, _directory) = native_mode();
    let calls = Arc::new(Mutex::new(Vec::new()));
    mode.set_session_host(Arc::new(SpyHost {
        calls: calls.clone(),
    }));
    mode.submit("/new", Default::default()).await.expect("new");
    assert_eq!(
        calls.lock().expect("calls").as_slice(),
        ["new_session"],
        "the mounted host receives /new instead of the local session"
    );
    mode.submit(
        &maho_interactive::interactive_mode::internal_command("/fork entry-1"),
        Default::default(),
    )
    .await
    .expect("fork");
    assert_eq!(
        calls.lock().expect("calls").as_slice(),
        ["new_session", "fork:entry-1:false"],
        "the mounted host receives /fork"
    );
    assert_eq!(
        mode.editor.editor.get_text(),
        "selected",
        "the host fork's selected text prefills the editor"
    );
    mode.submit("/export remote.html", Default::default())
        .await
        .expect("remote export");
    assert_eq!(
        calls.lock().expect("calls").last().map(String::as_str),
        Some("export_html:remote.html:dark")
    );
}

#[tokio::test]
async fn mounted_host_session_events_flow_into_the_mode() {
    use maho_interactive::interactive_host_runtime::SessionEventListener;
    use maho_interactive::interactive_session::{
        ForkOutcome, InteractiveSession, ReplacementOutcome, SessionFuture,
    };
    use maho_tui::tui::Component;
    use std::sync::{Arc, Mutex};

    struct BridgeHost {
        listener: Arc<Mutex<Option<SessionEventListener>>>,
    }
    impl InteractiveSession for BridgeHost {
        fn session_id(&self) -> Option<String> {
            None
        }
        fn session_file(&self) -> Option<String> {
            None
        }
        fn cwd(&self) -> String {
            String::new()
        }
        fn is_reconnecting(&self) -> bool {
            false
        }
        fn is_fallback(&self) -> bool {
            false
        }
        fn remote_state(
            &self,
        ) -> Option<maho_interactive::interactive_host_runtime::RemoteSessionState> {
            None
        }
        fn subscribe_session_events(
            &self,
            listener: SessionEventListener,
        ) -> Option<maho_interactive::interactive_host_runtime::SessionSubscription> {
            *self
                .listener
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(listener);
            None
        }
        fn request_remote_history(
            &self,
            _generation: u64,
            _sender: tokio::sync::mpsc::UnboundedSender<(
                u64,
                Result<Vec<serde_json::Value>, String>,
            )>,
        ) {
        }
        fn new_session(
            &self,
            _parent_session: Option<String>,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            Box::pin(async { Ok(ReplacementOutcome::Cancelled) })
        }
        fn switch_session(
            &self,
            _session_path: String,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            Box::pin(async { Ok(ReplacementOutcome::Cancelled) })
        }
        fn fork(
            &self,
            _entry_id: String,
            _include_entry: bool,
        ) -> SessionFuture<'_, Result<ForkOutcome, String>> {
            Box::pin(async {
                Ok(ForkOutcome {
                    outcome: ReplacementOutcome::Cancelled,
                    editor_text: None,
                })
            })
        }
        fn dispose(&self) -> SessionFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    let (mut mode, _directory) = native_mode();
    let listener = Arc::new(Mutex::new(None));
    mode.set_session_host(Arc::new(BridgeHost {
        listener: listener.clone(),
    }));
    let emit = listener
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .expect("the mode subscribed its event bridge");
    emit(&maho_ext_api::AgentSessionEvent::ContinuationError {
        error_message: "remote-bridge-error".into(),
    });
    assert!(
        mode.render(80).join("\n").contains("remote-bridge-error"),
        "the mounted host's session events drive the mode"
    );
}

#[tokio::test]
async fn manual_compact_runs_without_blocking_the_loop() {
    use maho_interactive::interactive_host_runtime::SessionEventListener;
    use maho_interactive::interactive_session::{
        ForkOutcome, InteractiveSession, ReplacementOutcome, SessionFuture,
    };
    use maho_tui::tui::Component;
    use std::sync::{Arc, Mutex};

    /// A host whose `compact` parks until `gate` is released, like a summarization request in flight.
    struct HeldCompactHost {
        listener: Arc<Mutex<Option<SessionEventListener>>>,
        gate: tokio::sync::watch::Receiver<bool>,
    }
    impl InteractiveSession for HeldCompactHost {
        fn session_id(&self) -> Option<String> {
            None
        }
        fn session_file(&self) -> Option<String> {
            None
        }
        fn cwd(&self) -> String {
            String::new()
        }
        fn is_reconnecting(&self) -> bool {
            false
        }
        fn is_fallback(&self) -> bool {
            false
        }
        fn remote_state(
            &self,
        ) -> Option<maho_interactive::interactive_host_runtime::RemoteSessionState> {
            None
        }
        fn subscribe_session_events(
            &self,
            listener: SessionEventListener,
        ) -> Option<maho_interactive::interactive_host_runtime::SessionSubscription> {
            *self
                .listener
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(listener);
            None
        }
        fn request_remote_history(
            &self,
            _generation: u64,
            _sender: tokio::sync::mpsc::UnboundedSender<(
                u64,
                Result<Vec<serde_json::Value>, String>,
            )>,
        ) {
        }
        fn compact(&self, _instructions: Option<String>) -> SessionFuture<'_, Result<(), String>> {
            let mut gate = self.gate.clone();
            Box::pin(async move {
                let _ = gate.wait_for(|released| *released).await;
                Ok(())
            })
        }
        fn new_session(
            &self,
            _parent_session: Option<String>,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            Box::pin(async { Ok(ReplacementOutcome::Cancelled) })
        }
        fn switch_session(
            &self,
            _session_path: String,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            Box::pin(async { Ok(ReplacementOutcome::Cancelled) })
        }
        fn fork(
            &self,
            _entry_id: String,
            _include_entry: bool,
        ) -> SessionFuture<'_, Result<ForkOutcome, String>> {
            Box::pin(async {
                Ok(ForkOutcome {
                    outcome: ReplacementOutcome::Cancelled,
                    editor_text: None,
                })
            })
        }
        fn dispose(&self) -> SessionFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    let (mut mode, _directory) = native_mode_with_session(None, |cwd| {
        let mut manager = maho_core::session_manager::SessionManager::in_memory(cwd, None, None);
        manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"first"}],"timestamp":0}));
        manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"second"}],"timestamp":0}));
        manager
    });
    let listener = Arc::new(Mutex::new(None));
    let (release, gate) = tokio::sync::watch::channel(false);
    mode.set_session_host(Arc::new(HeldCompactHost {
        listener: listener.clone(),
        gate,
    }));
    let (wake_tx, mut wake) = tokio::sync::mpsc::unbounded_channel();
    let waker = std::task::Waker::from(Arc::new(TurnWaker { wake: wake_tx }));
    mode.enqueue_submission("/compact", None);
    // The loop step returns while the compaction is held; before the fix it awaited it inline.
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        mode.pump_turn_with(&waker),
    )
    .await
    .expect("/compact must not block the loop step");
    assert!(
        mode.has_pending_compaction(),
        "the compaction is in flight, driven by later loop steps"
    );
    let emit = listener
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .expect("the mode subscribed its event bridge");
    emit(&maho_ext_api::AgentSessionEvent::CompactionStart {
        reason: maho_ext_api::CompactionReason::Manual,
        request_id: None,
    });
    mode.pump_turn_with(&waker).await;
    assert!(
        mode.render(80).join("\n").contains("Compacting context..."),
        "the live frame shows compaction progress while it runs"
    );
    mode.handle_input("typed");
    assert_eq!(
        mode.editor.editor.get_text(),
        "typed",
        "input is still consumed while compaction runs"
    );
    let _ = release.send(true);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            mode.pump_turn_with(&waker).await;
            if !mode.has_pending_compaction() {
                return;
            }
            let _ = wake.recv().await;
        }
    })
    .await
    .expect("the released compaction settles");
}

#[tokio::test]
async fn mounted_host_publishes_authoritative_remote_history_without_local_fallback() {
    use maho_interactive::interactive_host_runtime::{SessionEventListener, SessionSubscription};
    use maho_interactive::interactive_session::{
        ForkOutcome, InteractiveSession, ReplacementOutcome, SessionFuture,
    };
    use maho_tui::tui::Component;
    use std::sync::{Arc, Mutex};

    struct HistoryHost {
        listener: Arc<Mutex<Option<SessionEventListener>>>,
        generation: Arc<Mutex<Option<u64>>>,
        sender: HistorySender,
    }
    impl InteractiveSession for HistoryHost {
        fn session_id(&self) -> Option<String> {
            None
        }
        fn session_file(&self) -> Option<String> {
            None
        }
        fn cwd(&self) -> String {
            String::new()
        }
        fn is_reconnecting(&self) -> bool {
            false
        }
        fn is_fallback(&self) -> bool {
            false
        }
        fn remote_state(
            &self,
        ) -> Option<maho_interactive::interactive_host_runtime::RemoteSessionState> {
            None
        }
        fn subscribe_session_events(
            &self,
            listener: SessionEventListener,
        ) -> Option<SessionSubscription> {
            *self
                .listener
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(listener);
            None
        }
        fn request_remote_history(
            &self,
            generation: u64,
            sender: tokio::sync::mpsc::UnboundedSender<(
                u64,
                Result<Vec<serde_json::Value>, String>,
            )>,
        ) {
            *self
                .generation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(generation);
            *self
                .sender
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
        }
        fn new_session(
            &self,
            _parent_session: Option<String>,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            Box::pin(async { Ok(ReplacementOutcome::Replaced) })
        }
        fn switch_session(
            &self,
            _session_path: String,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            Box::pin(async { Ok(ReplacementOutcome::Replaced) })
        }
        fn fork(
            &self,
            _entry_id: String,
            _include_entry: bool,
        ) -> SessionFuture<'_, Result<ForkOutcome, String>> {
            Box::pin(async {
                Ok(ForkOutcome {
                    outcome: ReplacementOutcome::Replaced,
                    editor_text: None,
                })
            })
        }
        fn dispose(&self) -> SessionFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    type HostParts = (
        Arc<HistoryHost>,
        HistorySender,
        Arc<Mutex<Option<u64>>>,
        Arc<Mutex<Option<SessionEventListener>>>,
    );
    fn host() -> HostParts {
        let host = Arc::new(HistoryHost {
            listener: Arc::new(Mutex::new(None)),
            generation: Arc::new(Mutex::new(None)),
            sender: Arc::new(Mutex::new(None)),
        });
        (
            host.clone(),
            host.sender.clone(),
            host.generation.clone(),
            host.listener.clone(),
        )
    }

    let (mut mode, _directory) = native_mode_with_session(None, |cwd| {
        let mut manager = maho_core::session_manager::SessionManager::in_memory(cwd, None, None);
        manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"local-only"}],"timestamp":0}));
        manager
    });
    let (host, sender, generation, _listener) = host();
    mode.set_session_host(host);
    mode.rebuild_history();
    assert!(
        !mode.render(80).join("\n").contains("local-only"),
        "a mounted host never falls back to the local session"
    );
    let generation = generation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .expect("the mount requested history");
    sender.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().expect("the mount kept the sender").send((generation, Ok(vec![serde_json::json!({"role":"user","content":[{"type":"text","text":"remote-history"}],"timestamp":0})]))).expect("send history");
    mode.drain_events();
    let rendered = mode.render(80).join("\n");
    assert!(
        rendered.contains("remote-history"),
        "the published snapshot rebuilds the transcript: {rendered}"
    );
    assert!(
        !rendered.contains("local-only"),
        "the local session is not mixed in"
    );
}

#[tokio::test]
async fn stale_history_fetch_completion_is_discarded() {
    use maho_interactive::interactive_host_runtime::{SessionEventListener, SessionSubscription};
    use maho_interactive::interactive_session::{
        ForkOutcome, InteractiveSession, ReplacementOutcome, SessionFuture,
    };
    use maho_tui::tui::Component;
    use std::sync::{Arc, Mutex};

    struct StaleHost {
        generation: Arc<Mutex<Option<u64>>>,
        sender: HistorySender,
    }
    impl InteractiveSession for StaleHost {
        fn session_id(&self) -> Option<String> {
            None
        }
        fn session_file(&self) -> Option<String> {
            None
        }
        fn cwd(&self) -> String {
            String::new()
        }
        fn is_reconnecting(&self) -> bool {
            false
        }
        fn is_fallback(&self) -> bool {
            false
        }
        fn remote_state(
            &self,
        ) -> Option<maho_interactive::interactive_host_runtime::RemoteSessionState> {
            None
        }
        fn subscribe_session_events(
            &self,
            _listener: SessionEventListener,
        ) -> Option<SessionSubscription> {
            None
        }
        fn request_remote_history(
            &self,
            generation: u64,
            sender: tokio::sync::mpsc::UnboundedSender<(
                u64,
                Result<Vec<serde_json::Value>, String>,
            )>,
        ) {
            *self
                .generation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(generation);
            *self
                .sender
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
        }
        fn new_session(
            &self,
            _parent_session: Option<String>,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            Box::pin(async { Ok(ReplacementOutcome::Replaced) })
        }
        fn switch_session(
            &self,
            _session_path: String,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            Box::pin(async { Ok(ReplacementOutcome::Replaced) })
        }
        fn fork(
            &self,
            _entry_id: String,
            _include_entry: bool,
        ) -> SessionFuture<'_, Result<ForkOutcome, String>> {
            Box::pin(async {
                Ok(ForkOutcome {
                    outcome: ReplacementOutcome::Replaced,
                    editor_text: None,
                })
            })
        }
        fn dispose(&self) -> SessionFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    let (mut mode, _directory) = native_mode();
    let generation = Arc::new(Mutex::new(None));
    let sender = Arc::new(Mutex::new(None));
    mode.set_session_host(Arc::new(StaleHost {
        generation: generation.clone(),
        sender: sender.clone(),
    }));
    let first = generation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .expect("first request");
    mode.submit("/new", Default::default())
        .await
        .expect("replacement");
    let second = generation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .expect("replacement request");
    assert!(second > first, "a replacement bumps the generation");
    sender.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().expect("sender").send((first, Ok(vec![serde_json::json!({"role":"user","content":[{"type":"text","text":"stale-history"}],"timestamp":0})]))).expect("send stale");
    mode.drain_events();
    assert!(
        !mode.render(80).join("\n").contains("stale-history"),
        "a completion for a superseded generation is discarded"
    );
}

#[tokio::test]
async fn remote_live_event_during_history_fetch_is_still_applied() {
    use maho_interactive::interactive_host_runtime::{SessionEventListener, SessionSubscription};
    use maho_interactive::interactive_session::{
        ForkOutcome, InteractiveSession, ReplacementOutcome, SessionFuture,
    };
    use maho_tui::tui::Component;
    use std::sync::{Arc, Mutex};

    struct LiveHost {
        listener: Arc<Mutex<Option<SessionEventListener>>>,
        generation: Arc<Mutex<Option<u64>>>,
        sender: HistorySender,
    }
    impl InteractiveSession for LiveHost {
        fn session_id(&self) -> Option<String> {
            None
        }
        fn session_file(&self) -> Option<String> {
            None
        }
        fn cwd(&self) -> String {
            String::new()
        }
        fn is_reconnecting(&self) -> bool {
            false
        }
        fn is_fallback(&self) -> bool {
            false
        }
        fn remote_state(
            &self,
        ) -> Option<maho_interactive::interactive_host_runtime::RemoteSessionState> {
            None
        }
        fn subscribe_session_events(
            &self,
            listener: SessionEventListener,
        ) -> Option<SessionSubscription> {
            *self
                .listener
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(listener);
            None
        }
        fn request_remote_history(
            &self,
            generation: u64,
            sender: tokio::sync::mpsc::UnboundedSender<(
                u64,
                Result<Vec<serde_json::Value>, String>,
            )>,
        ) {
            *self
                .generation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(generation);
            *self
                .sender
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
        }
        fn new_session(
            &self,
            _parent_session: Option<String>,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            Box::pin(async { Ok(ReplacementOutcome::Cancelled) })
        }
        fn switch_session(
            &self,
            _session_path: String,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            Box::pin(async { Ok(ReplacementOutcome::Cancelled) })
        }
        fn fork(
            &self,
            _entry_id: String,
            _include_entry: bool,
        ) -> SessionFuture<'_, Result<ForkOutcome, String>> {
            Box::pin(async {
                Ok(ForkOutcome {
                    outcome: ReplacementOutcome::Cancelled,
                    editor_text: None,
                })
            })
        }
        fn dispose(&self) -> SessionFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    let (mut mode, _directory) = native_mode();
    let listener = Arc::new(Mutex::new(None));
    let generation = Arc::new(Mutex::new(None));
    let sender = Arc::new(Mutex::new(None));
    mode.set_session_host(Arc::new(LiveHost {
        listener: listener.clone(),
        generation: generation.clone(),
        sender: sender.clone(),
    }));
    let emit = listener
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .expect("bridge subscribed");
    emit(&maho_ext_api::AgentSessionEvent::ContinuationError {
        error_message: "live-during-fetch".into(),
    });
    let generation = generation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .expect("request");
    sender.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().expect("sender").send((generation, Ok(vec![serde_json::json!({"role":"user","content":[{"type":"text","text":"remote-history"}],"timestamp":0})]))).expect("send history");
    mode.drain_events();
    let rendered = mode.render(80).join("\n");
    assert!(
        rendered.contains("remote-history"),
        "the snapshot is published: {rendered}"
    );
    assert!(
        rendered.contains("live-during-fetch"),
        "a live event buffered during the fetch is applied after the snapshot: {rendered}"
    );
}

#[tokio::test]
async fn mounting_a_host_disables_local_event_delivery() {
    use maho_interactive::interactive_host_runtime::{SessionEventListener, SessionSubscription};
    use maho_interactive::interactive_session::{
        ForkOutcome, InteractiveSession, ReplacementOutcome, SessionFuture,
    };
    use std::sync::Arc;

    struct GateHost;
    impl InteractiveSession for GateHost {
        fn session_id(&self) -> Option<String> {
            None
        }
        fn session_file(&self) -> Option<String> {
            None
        }
        fn cwd(&self) -> String {
            String::new()
        }
        fn is_reconnecting(&self) -> bool {
            false
        }
        fn is_fallback(&self) -> bool {
            false
        }
        fn remote_state(
            &self,
        ) -> Option<maho_interactive::interactive_host_runtime::RemoteSessionState> {
            None
        }
        fn subscribe_session_events(
            &self,
            _listener: SessionEventListener,
        ) -> Option<SessionSubscription> {
            None
        }
        fn request_remote_history(
            &self,
            _generation: u64,
            _sender: tokio::sync::mpsc::UnboundedSender<(
                u64,
                Result<Vec<serde_json::Value>, String>,
            )>,
        ) {
        }
        fn new_session(
            &self,
            _parent_session: Option<String>,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            Box::pin(async { Ok(ReplacementOutcome::Cancelled) })
        }
        fn switch_session(
            &self,
            _session_path: String,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            Box::pin(async { Ok(ReplacementOutcome::Cancelled) })
        }
        fn fork(
            &self,
            _entry_id: String,
            _include_entry: bool,
        ) -> SessionFuture<'_, Result<ForkOutcome, String>> {
            Box::pin(async {
                Ok(ForkOutcome {
                    outcome: ReplacementOutcome::Cancelled,
                    editor_text: None,
                })
            })
        }
        fn dispose(&self) -> SessionFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    let (mut mode, _directory) = native_mode();
    assert!(
        mode.local_events_enabled(),
        "before mounting, the local session drives the view"
    );
    mode.set_session_host(Arc::new(GateHost));
    assert!(
        !mode.local_events_enabled(),
        "a mounted host suppresses local session events"
    );
}

#[tokio::test]
async fn mirrored_remote_state_drives_the_mode_reads() {
    use maho_interactive::interactive_host_runtime::{
        RemoteSessionState, SessionEventListener, SessionSubscription,
    };
    use maho_interactive::interactive_session::{
        ForkOutcome, InteractiveSession, ReplacementOutcome, SessionFuture,
    };
    use maho_tui::tui::Component;
    use std::sync::Arc;

    struct StateHost;
    impl InteractiveSession for StateHost {
        fn session_id(&self) -> Option<String> {
            None
        }
        fn session_file(&self) -> Option<String> {
            None
        }
        fn cwd(&self) -> String {
            String::new()
        }
        fn is_reconnecting(&self) -> bool {
            false
        }
        fn is_fallback(&self) -> bool {
            false
        }
        fn remote_state(&self) -> Option<RemoteSessionState> {
            Some(RemoteSessionState {
                cwd: Some("/remote/cwd".into()),
                session_name: Some("remote-name".into()),
                is_streaming: true,
                auto_compaction_enabled: true,
                ..Default::default()
            })
        }
        fn subscribe_session_events(
            &self,
            _listener: SessionEventListener,
        ) -> Option<SessionSubscription> {
            None
        }
        fn request_remote_history(
            &self,
            _generation: u64,
            _sender: tokio::sync::mpsc::UnboundedSender<(
                u64,
                Result<Vec<serde_json::Value>, String>,
            )>,
        ) {
        }
        fn new_session(
            &self,
            _parent_session: Option<String>,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            Box::pin(async { Ok(ReplacementOutcome::Cancelled) })
        }
        fn switch_session(
            &self,
            _session_path: String,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            Box::pin(async { Ok(ReplacementOutcome::Cancelled) })
        }
        fn fork(
            &self,
            _entry_id: String,
            _include_entry: bool,
        ) -> SessionFuture<'_, Result<ForkOutcome, String>> {
            Box::pin(async {
                Ok(ForkOutcome {
                    outcome: ReplacementOutcome::Cancelled,
                    editor_text: None,
                })
            })
        }
        fn dispose(&self) -> SessionFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    let (mut mode, _directory) = native_mode();
    mode.set_session_host(Arc::new(StateHost));
    mode.submit("/session", Default::default())
        .await
        .expect("session info");
    let rendered = mode.render(80).join("\n");
    assert!(
        rendered.contains("remote-name"),
        "the mirrored session name drives the read: {rendered}"
    );
}

#[tokio::test]
async fn turn_and_session_mutations_route_to_the_mounted_host() {
    use maho_interactive::interactive_host_runtime::{
        RemoteSessionState, SessionEventListener, SessionSubscription,
    };
    use maho_interactive::interactive_session::{
        ForkOutcome, InteractiveSession, ReplacementOutcome, SessionFuture,
    };
    use std::sync::{Arc, Mutex};

    struct RecordingHost {
        calls: Arc<Mutex<Vec<String>>>,
    }
    impl InteractiveSession for RecordingHost {
        fn session_id(&self) -> Option<String> {
            None
        }
        fn session_file(&self) -> Option<String> {
            None
        }
        fn cwd(&self) -> String {
            String::new()
        }
        fn is_reconnecting(&self) -> bool {
            false
        }
        fn is_fallback(&self) -> bool {
            false
        }
        fn remote_state(&self) -> Option<RemoteSessionState> {
            Some(RemoteSessionState::default())
        }
        fn subscribe_session_events(
            &self,
            _listener: SessionEventListener,
        ) -> Option<SessionSubscription> {
            None
        }
        fn request_remote_history(
            &self,
            _generation: u64,
            _sender: tokio::sync::mpsc::UnboundedSender<(
                u64,
                Result<Vec<serde_json::Value>, String>,
            )>,
        ) {
        }
        fn request_remote_models(
            &self,
            _generation: u64,
            _sender: tokio::sync::mpsc::UnboundedSender<(
                u64,
                Result<Vec<serde_json::Value>, String>,
            )>,
        ) {
        }
        fn request_remote_stats(
            &self,
            _generation: u64,
            _sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<serde_json::Value, String>)>,
        ) {
        }
        fn prompt(
            &self,
            _message: String,
            _options: maho_core::agent_session::PromptOptions,
        ) -> SessionFuture<'_, Result<(), String>> {
            let calls = self.calls.clone();
            Box::pin(async move {
                calls
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push("prompt".into());
                Ok(())
            })
        }
        fn abort(&self) -> SessionFuture<'_, Result<(), String>> {
            let calls = self.calls.clone();
            Box::pin(async move {
                calls
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push("abort".into());
                Ok(())
            })
        }
        fn steer(&self, _text: String) -> SessionFuture<'_, Result<(), String>> {
            let calls = self.calls.clone();
            Box::pin(async move {
                calls
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push("steer".into());
                Ok(())
            })
        }
        fn follow_up(&self, _text: String) -> SessionFuture<'_, Result<(), String>> {
            let calls = self.calls.clone();
            Box::pin(async move {
                calls
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push("follow_up".into());
                Ok(())
            })
        }
        fn compact(&self, _instructions: Option<String>) -> SessionFuture<'_, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
        fn navigate_tree(
            &self,
            _entry_id: String,
            _options: maho_core::agent_session::TreeNavigationOptions,
        ) -> SessionFuture<'_, Result<maho_core::agent_session::AssistantEditResult, String>>
        {
            Box::pin(async { Ok(maho_core::agent_session::AssistantEditResult::default()) })
        }
        fn edit_assistant_message(
            &self,
            _entry_id: String,
            _text: String,
            _options: maho_core::agent_session::TreeNavigationOptions,
        ) -> SessionFuture<'_, Result<maho_core::agent_session::AssistantEditResult, String>>
        {
            Box::pin(async { Ok(maho_core::agent_session::AssistantEditResult::default()) })
        }
        fn reload(&self) -> SessionFuture<'_, Result<maho_ext_api::ReloadVetoDecision, String>> {
            Box::pin(async { Ok(maho_ext_api::ReloadVetoDecision::default()) })
        }
        fn execute_bash(
            &self,
            _command: String,
            _exclude_from_context: bool,
        ) -> SessionFuture<'_, Result<serde_json::Value, String>> {
            Box::pin(async { Ok(serde_json::json!({})) })
        }
        fn set_model(
            &self,
            _provider: String,
            _id: String,
        ) -> SessionFuture<'_, Result<(), String>> {
            let calls = self.calls.clone();
            Box::pin(async move {
                calls
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push("set_model".into());
                Ok(())
            })
        }
        fn set_session_name(&self, _name: String) -> SessionFuture<'_, Result<(), String>> {
            let calls = self.calls.clone();
            Box::pin(async move {
                calls
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push("set_session_name".into());
                Ok(())
            })
        }
        fn set_session_thinking_level(
            &self,
            _level: String,
        ) -> SessionFuture<'_, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
        fn cycle_thinking_level(&self) -> SessionFuture<'_, Result<Option<String>, String>> {
            Box::pin(async { Ok(None) })
        }
        fn cycle_model(&self, _forward: bool) -> SessionFuture<'_, Result<Option<String>, String>> {
            Box::pin(async { Ok(None) })
        }
        fn export_jsonl(
            &self,
            _output_path: Option<String>,
        ) -> SessionFuture<'_, Result<Option<String>, String>> {
            Box::pin(async { Ok(None) })
        }
        fn new_session(
            &self,
            _parent_session: Option<String>,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            Box::pin(async { Ok(ReplacementOutcome::Cancelled) })
        }
        fn switch_session(
            &self,
            _session_path: String,
        ) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            Box::pin(async { Ok(ReplacementOutcome::Cancelled) })
        }
        fn fork(
            &self,
            _entry_id: String,
            _include_entry: bool,
        ) -> SessionFuture<'_, Result<ForkOutcome, String>> {
            Box::pin(async {
                Ok(ForkOutcome {
                    outcome: ReplacementOutcome::Cancelled,
                    editor_text: None,
                })
            })
        }
        fn dispose(&self) -> SessionFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    let (mut mode, _directory) = native_mode();
    let calls = Arc::new(Mutex::new(Vec::new()));
    mode.set_session_host(Arc::new(RecordingHost {
        calls: calls.clone(),
    }));
    mode.submit("hello there", Default::default())
        .await
        .expect("prompt");
    mode.abort().await;
    mode.steer("steer text").await.expect("steer");
    mode.follow_up("follow text").await.expect("follow_up");
    mode.submit("/rename renamed", Default::default())
        .await
        .expect("rename");
    let recorded = calls
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(
        recorded.iter().any(|call| call == "prompt"),
        "prompt routes to the host: {recorded:?}"
    );
    assert!(
        recorded.iter().any(|call| call == "abort"),
        "abort routes to the host: {recorded:?}"
    );
    assert!(
        recorded.iter().any(|call| call == "steer"),
        "steer routes to the host: {recorded:?}"
    );
    assert!(
        recorded.iter().any(|call| call == "follow_up"),
        "follow_up routes to the host: {recorded:?}"
    );
    assert!(
        recorded.iter().any(|call| call == "set_session_name"),
        "/rename routes to the host: {recorded:?}"
    );
}

// ---- task-1 real-loop dispatch (GAP-1) --------------------------------------------------------

/// A waker that hands a token to a channel on every wake, so an event-driven driver re-polls the
/// turn exactly when the in-flight turn or abort future makes progress (no fixed sleeps, no
/// state-polling spin).
struct TurnWaker {
    wake: tokio::sync::mpsc::UnboundedSender<()>,
}
impl std::task::Wake for TurnWaker {
    fn wake(self: std::sync::Arc<Self>) {
        let _ = self.wake.send(());
    }
    fn wake_by_ref(self: &std::sync::Arc<Self>) {
        let _ = self.wake.send(());
    }
}

/// A mode whose faux provider parks on the first streamed chunk until `gate` is released: the turn
/// reaches `AgentStart` and then blocks, so a test can observe the live frame and steer/abort while
/// the turn is genuinely in flight. `entered` flips once the provider is parked, and `wake`/`wake_tx`
/// carry the turn's wakeups to the causal driver.
struct GatedMode {
    mode: maho_interactive::interactive_mode::InteractiveMode,
    session: std::sync::Arc<maho_core::agent_session::AgentSession>,
    gate: tokio::sync::watch::Sender<bool>,
    wake: tokio::sync::mpsc::UnboundedReceiver<()>,
    wake_tx: tokio::sync::mpsc::UnboundedSender<()>,
    entered: std::sync::Arc<std::sync::atomic::AtomicBool>,
    _directory: tempfile::TempDir,
}

fn gated_native_mode() -> GatedMode {
    use maho_ai::providers::faux::{
        FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message,
        faux_provider, faux_streams,
    };
    use maho_core::agent_session::{AgentSession, AgentSessionConfig};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    let directory = tempfile::tempdir().expect("directory");
    let cwd = directory.path().to_string_lossy().into_owned();
    let (gate, gate_rx) = tokio::sync::watch::channel(false);
    let entered = Arc::new(AtomicBool::new(false));
    let (wake_tx, wake) = tokio::sync::mpsc::unbounded_channel();
    let hook_entered = entered.clone();
    let hook_gate = gate_rx.clone();
    let provider = faux_provider(RegisterFauxProviderOptions {
        api: Some("faux".into()),
        tokens_per_second: Some(0.0),
        scheduler_hook: Some(Arc::new(move || {
            let entered = hook_entered.clone();
            let mut rx = hook_gate.clone();
            Box::pin(async move {
                entered.store(true, std::sync::atomic::Ordering::SeqCst);
                let _ = rx.wait_for(|released| *released).await;
            })
        })),
        ..Default::default()
    })
    .expect("valid faux provider");
    let model = provider.get_model(Some("faux-1")).expect("model");
    provider.set_responses(vec![
        faux_assistant_message(
            "hello",
            FauxAssistantMessageOptions {
                timestamp: Some(0),
                ..Default::default()
            },
        )
        .into(),
        faux_assistant_message(
            "steered-reply",
            FauxAssistantMessageOptions {
                timestamp: Some(0),
                ..Default::default()
            },
        )
        .into(),
    ]);
    let streams = faux_streams(provider.core.clone());
    let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| {
        let mut context = context.clone();
        context.system_prompt = context.system_prompt.filter(|prompt| !prompt.is_empty());
        streams.stream_simple(model, &context, options.map(|options| options.simple))
    });
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(
        maho_core::model_runtime::CreateModelRuntimeOptions {
            models_path: Some(directory.path().join("models.json")),
            auth_path: Some(directory.path().join("auth.json")),
            providers: Some(vec![provider.provider.clone()]),
            ..Default::default()
        },
    )
    .expect("valid model runtime");
    let session = Arc::new(
        AgentSession::new(AgentSessionConfig {
            agent: maho_agent::Agent::new(maho_agent::AgentOptions {
                initial_state: Some(maho_agent::agent::PartialAgentState {
                    model: Some(model),
                    ..Default::default()
                }),
                stream_fn: Some(stream_fn),
                ..Default::default()
            }),
            session_manager: maho_core::session_manager::SessionManager::in_memory(
                &cwd, None, None,
            ),
            settings_manager: maho_core::settings_manager::SettingsManager::from_storage(
                Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),
                false,
            ),
            cwd: cwd.clone(),
            agent_dir: Some(cwd),
            fallback_now: Some(Arc::new(|| 0.0)),
            retry_random: Some(Arc::new(|| 0.5)),
            scoped_models: Vec::new(),
            favorite_models: Vec::new(),
            flag_values: Default::default(),
            custom_tools: Vec::new(),
            model_runtime: Some(runtime),
            model_registry: None,
            uses_default_stream_function: Some(false),
            initial_active_tool_names: None,
            default_tool_names: None,
            eval_only_tool_names: None,
            allowed_tool_names: None,
            excluded_tool_names: None,
            base_tools_override: None,
            session_start_event: None,
            auto_title_sessions: Some(false),
        })
        .expect("session"),
    );
    let mode = maho_interactive::interactive_mode::InteractiveMode::new(
        session.clone(),
        maho_interactive::theme::Theme::builtin(
            "dark",
            maho_interactive::theme::ColorMode::Truecolor,
        )
        .expect("theme"),
        std::rc::Rc::new(EditorHost),
    );
    GatedMode {
        mode,
        session,
        gate,
        wake,
        wake_tx,
        entered,
        _directory: directory,
    }
}

/// Capture the session's exact event stream while the mode drives the turn.
fn capture_session_events(
    session: &std::sync::Arc<maho_core::agent_session::AgentSession>,
) -> (
    std::sync::Arc<std::sync::Mutex<Vec<maho_ext_api::AgentSessionEvent>>>,
    maho_core::agent_session::AgentSessionSubscription,
) {
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = events.clone();
    let subscription = session.subscribe(std::sync::Arc::new(move |event| {
        sink.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.clone())
    }));
    (events, subscription)
}

fn is_terminal_session_event(event: &maho_ext_api::AgentSessionEvent) -> bool {
    matches!(
        event,
        maho_ext_api::AgentSessionEvent::AgentSettled
            | maho_ext_api::AgentSessionEvent::AgentIdle
            | maho_ext_api::AgentSessionEvent::SessionAbort
    ) || matches!(
        event,
        maho_ext_api::AgentSessionEvent::AgentEnd { .. }
            | maho_ext_api::AgentSessionEvent::Agent(
                maho_agent::types::AgentEvent::AgentEnd { .. }
            )
    )
}

fn abort_count(events: &std::sync::Mutex<Vec<maho_ext_api::AgentSessionEvent>>) -> usize {
    events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|event| matches!(event, maho_ext_api::AgentSessionEvent::SessionAbort))
        .count()
}

fn has_settled(events: &std::sync::Mutex<Vec<maho_ext_api::AgentSessionEvent>>) -> bool {
    events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|event| matches!(event, maho_ext_api::AgentSessionEvent::AgentSettled))
}

fn has_agent_start(events: &std::sync::Mutex<Vec<maho_ext_api::AgentSessionEvent>>) -> bool {
    events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|event| {
            matches!(
                event,
                maho_ext_api::AgentSessionEvent::Agent(maho_agent::types::AgentEvent::AgentStart)
            )
        })
}

/// The causal waker over `gated`'s wake channel.
fn turn_waker(gated: &GatedMode) -> std::task::Waker {
    std::task::Waker::from(std::sync::Arc::new(TurnWaker {
        wake: gated.wake_tx.clone(),
    }))
}

/// Drive the production loop causally: poll once, then re-poll only when the in-flight turn or
/// abort future wakes (each wake is one token on `gated.wake`), until `predicate` holds. Bounded; a
/// hang fails rather than passing by luck. The caller subscribes to the session event stream before
/// the trigger, so the terminal event is captured, never inferred from polling mode state.
async fn pump_until(gated: &mut GatedMode, predicate: impl Fn(&GatedMode) -> bool) {
    let waker = turn_waker(gated);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            gated.mode.pump_turn_with(&waker).await;
            // The production loop polls a newly started turn only after paint.
            gated.mode.poll_after_paint(&waker);
            if predicate(gated) {
                return;
            }
            let _ = gated.wake.recv().await;
        }
    })
    .await
    .expect("the loop reaches the expected state before the bounded timeout");
}

#[tokio::test]
async fn real_loop_renders_live_frame_before_held_delta_then_completes() {
    use maho_tui::tui::Component;
    let mut gated = gated_native_mode();
    let (events, _subscription) = capture_session_events(&gated.session);
    gated.mode.handle_input("hi");
    gated.mode.handle_input("\r");
    pump_until(&mut gated, |gated| {
        gated.entered.load(std::sync::atomic::Ordering::SeqCst)
    })
    .await;
    assert!(
        gated.mode.has_pending_turn(),
        "the entered prompt starts a turn"
    );
    assert!(!gated.mode.agent_idle, "the turn is in flight");
    assert!(
        gated.mode.working_frame(0.0).is_some(),
        "the live frame shows the working row while the provider holds"
    );
    assert!(
        has_agent_start(&events),
        "the provider-held run started before the delta"
    );
    let frame = gated.mode.render(80).join("\n");
    assert!(
        !frame.contains("hello"),
        "no reply before the held delta is released: {frame}"
    );
    let _ = gated.gate.send(true);
    pump_until(&mut gated, |gated| !gated.mode.has_pending_turn()).await;
    assert!(gated.mode.agent_idle, "the turn reaches its terminal event");
    assert!(
        gated.mode.working_frame(0.0).is_none(),
        "the working row is cleared"
    );
    let frame = gated.mode.render(80).join("\n");
    assert!(
        frame.contains("hello"),
        "the reply is rendered after the delta: {frame}"
    );
    let captured = events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(
        captured.last().is_some_and(is_terminal_session_event),
        "exact terminal event: {captured:?}"
    );
    assert!(has_settled(&events), "the run settled: {captured:?}");
    assert_eq!(abort_count(&events), 0, "a completed turn is not an abort");
}

#[tokio::test]
async fn real_loop_keeps_keyword_glow_until_the_keyword_turn_ends() {
    use maho_tui::tui::Component;
    maho_interactive::components::neon_border::register_glow_keywords(&["spk"]);
    let mut gated = gated_native_mode();
    let heavy_borders = |gated: &mut GatedMode| {
        gated
            .mode
            .render(80)
            .iter()
            .filter(|line| maho_tui::utils::strip_terminal_sequences(line).starts_with('━'))
            .count()
    };
    gated.mode.handle_input("spk hi");
    assert_eq!(
        heavy_borders(&mut gated),
        2,
        "typing a glow keyword lights the editor border"
    );
    gated.mode.handle_input("\r");
    pump_until(&mut gated, |gated| {
        gated.entered.load(std::sync::atomic::Ordering::SeqCst)
    })
    .await;
    assert!(
        gated.mode.editor.editor.get_text().is_empty(),
        "the submitted draft left the editor"
    );
    assert_eq!(
        heavy_borders(&mut gated),
        2,
        "the glow stays while the keyword turn runs"
    );
    let _ = gated.gate.send(true);
    pump_until(&mut gated, |gated| !gated.mode.has_pending_turn()).await;
    assert_eq!(heavy_borders(&mut gated), 0, "the glow ends with the turn");
}

#[tokio::test]
async fn real_loop_abort_while_provider_held_clears_busy_and_returns_usable_editor() {
    use maho_tui::tui::Component;
    let mut gated = gated_native_mode();
    let (events, _subscription) = capture_session_events(&gated.session);
    gated.mode.handle_input("hi");
    gated.mode.handle_input("\r");
    pump_until(&mut gated, |gated| {
        gated.entered.load(std::sync::atomic::Ordering::SeqCst)
    })
    .await;
    assert!(gated.mode.has_pending_turn());
    // The pinned interrupt key (`app.interrupt`) requests the abort; it must not block the loop. The
    // provider gate stays held, so the abort itself must tear the held stream down.
    gated
        .mode
        .handle_runtime_input("\x1b", 0)
        .await
        .expect("interrupt");
    assert!(
        gated.mode.has_pending_turn(),
        "the abort is requested, not awaited inline"
    );
    pump_until(&mut gated, |gated| !gated.mode.has_pending_turn()).await;
    assert!(
        gated.mode.agent_idle,
        "abort clears the busy state without the provider succeeding"
    );
    assert!(
        gated.mode.working_frame(0.0).is_none(),
        "the working row is cleared"
    );
    let frame = gated.mode.render(80).join("\n");
    assert!(
        !frame.contains("hello"),
        "the abort beat the held delta: {frame}"
    );
    let captured = events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(
        captured.last().is_some_and(is_terminal_session_event),
        "exact terminal event: {captured:?}"
    );
    assert!(
        has_settled(&events),
        "the aborted run settles: {captured:?}"
    );
    let provider_succeeded = gated
        .session
        .messages()
        .iter()
        .filter_map(|message| message.as_assistant())
        .any(|message| message.stop_reason == maho_ai::types::StopReason::Stop);
    assert!(
        !provider_succeeded,
        "the held provider never completed a successful reply"
    );
    gated.mode.handle_input("next");
    assert_eq!(
        gated.mode.editor.editor.get_text(),
        "next",
        "the editor accepts input again"
    );
}

#[tokio::test]
async fn real_loop_steer_while_provider_held_queues_without_replacing_the_turn() {
    use maho_tui::tui::Component;
    let mut gated = gated_native_mode();
    gated.mode.handle_input("hi");
    gated.mode.handle_input("\r");
    pump_until(&mut gated, |gated| {
        gated.entered.load(std::sync::atomic::Ordering::SeqCst)
    })
    .await;
    assert!(gated.mode.has_pending_turn());
    // Type a follow-up and submit it while the turn is held: it must queue, not replace the turn.
    gated.mode.handle_input("steer-now");
    gated.mode.handle_input("\r");
    let waker = turn_waker(&gated);
    gated.mode.pump_turn_with(&waker).await;
    // The enqueue is parked until after paint, then applied in order.
    gated.mode.poll_after_paint(&waker);
    assert!(
        gated.mode.has_pending_turn(),
        "the steer is queued into the running turn"
    );
    assert!(!gated.mode.agent_idle, "the running turn is not replaced");
    let _ = gated.gate.send(true);
    pump_until(&mut gated, |gated| !gated.mode.has_pending_turn()).await;
    assert!(gated.mode.agent_idle);
    let frame = gated.mode.render(80).join("\n");
    assert!(
        frame.contains("steered-reply"),
        "the queued steer ran after the held turn: {frame}"
    );
}

#[tokio::test]
async fn real_loop_pumps_startup_initial_prompt_without_freezing_the_first_frame() {
    use maho_tui::tui::Component;
    let mut gated = gated_native_mode();
    let (events, _subscription) = capture_session_events(&gated.session);
    // The production entry enqueues the startup prompt instead of awaiting it before the loop.
    gated.mode.enqueue_submission("startup-hi", None);
    pump_until(&mut gated, |gated| {
        gated.entered.load(std::sync::atomic::Ordering::SeqCst)
    })
    .await;
    assert!(
        gated.mode.has_pending_turn(),
        "the startup prompt starts a turn inside the loop"
    );
    assert!(
        gated.mode.working_frame(0.0).is_some(),
        "the first frame renders while the startup turn is held"
    );
    assert!(
        !gated.mode.render(80).join("\n").contains("hello"),
        "no reply before the held delta"
    );
    let _ = gated.gate.send(true);
    pump_until(&mut gated, |gated| !gated.mode.has_pending_turn()).await;
    assert!(gated.mode.agent_idle);
    assert!(
        gated.mode.render(80).join("\n").contains("hello"),
        "the startup reply renders after the delta"
    );
    let captured = events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(
        captured.last().is_some_and(is_terminal_session_event),
        "exact terminal event: {captured:?}"
    );
}

#[tokio::test]
async fn double_escape_on_empty_editor_opens_tree_within_the_window() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let opened =
        |frame: String| frame.contains("Session Tree") || frame.contains("No entries in session");
    mode.handle_input_at("\x1b", 1000);
    assert!(
        !opened(mode.render(80).join("\n")),
        "a single escape only arms the window"
    );
    mode.handle_input_at("\x1b", 1499);
    // /tree loads the session tree in spawn_blocking; the selector mounts when the parked load settles.
    assert!(
        !opened(mode.render(80).join("\n")),
        "the tree load is parked, not run inline"
    );
    let (wake_tx, mut wake) = tokio::sync::mpsc::unbounded_channel();
    let waker = std::task::Waker::from(std::sync::Arc::new(TurnWaker { wake: wake_tx }));
    loop {
        mode.poll_after_paint(&waker);
        if opened(mode.render(80).join("\n")) {
            break;
        }
        tokio::time::timeout(std::time::Duration::from_secs(10), wake.recv())
            .await
            .expect(
                "the second escape inside 500ms runs /tree and the parked load settles within 10s",
            )
            .expect("the parked tree load wakes the frame");
    }
}

#[test]
fn escape_clears_a_bash_mode_draft() {
    let (mut mode, _directory) = native_mode();
    mode.editor.editor.set_text("!ls");
    mode.handle_input_at("\x1b", 0);
    assert!(mode.editor.editor.get_text().is_empty());
}

#[tokio::test]
async fn alt_enter_when_idle_submits_like_enter() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.handle_input("hi");
    mode.handle_runtime_input(if cfg!(windows) { "\x11" } else { "\x1b\r" }, 0)
        .await
        .expect("follow-up key");
    assert!(
        mode.editor.editor.get_text().is_empty(),
        "the draft is cleared"
    );
    assert_eq!(
        mode.submit_editor().await.expect("submit"),
        Some(maho_core::agent_session::PromptDisposition::Started)
    );
    assert!(mode.render(80).join("\n").contains("hello"));
}

#[tokio::test]
async fn alt_enter_while_streaming_queues_a_follow_up() {
    use maho_tui::tui::Component;
    let mut gated = gated_native_mode();
    gated.mode.handle_input("hi");
    gated.mode.handle_input("\r");
    pump_until(&mut gated, |gated| {
        gated.entered.load(std::sync::atomic::Ordering::SeqCst)
    })
    .await;
    gated.mode.handle_input("later");
    gated
        .mode
        .handle_runtime_input(if cfg!(windows) { "\x11" } else { "\x1b\r" }, 0)
        .await
        .expect("follow-up key");
    assert!(
        gated.mode.editor.editor.get_text().is_empty(),
        "the draft is cleared"
    );
    // The enqueue is parked so the cleared editor can paint before `prompt` runs.
    gated.mode.poll_after_paint(std::task::Waker::noop());
    assert_eq!(gated.session.get_follow_up_messages(), ["later"]);
    let _ = gated.gate.send(true);
    pump_until(&mut gated, |gated| !gated.mode.has_pending_turn()).await;
}

#[tokio::test]
async fn suspend_and_external_editor_keys_request_a_terminal_hand_off() {
    use maho_interactive::interactive_mode::TerminalRequest;
    let (mut mode, _directory) = native_mode();
    mode.handle_runtime_input("\x07", 0).await.expect("ctrl+g");
    assert_eq!(
        mode.take_terminal_request(),
        Some(TerminalRequest::ExternalEditor)
    );
    mode.handle_runtime_input("\x1a", 1).await.expect("ctrl+z");
    assert_eq!(
        mode.take_terminal_request(),
        if cfg!(windows) {
            None
        } else {
            Some(TerminalRequest::Suspend)
        }
    );
    assert_eq!(
        mode.take_terminal_request(),
        None,
        "a request is consumed once"
    );
}

#[tokio::test]
async fn configured_resume_shortcut_opens_session_selector() {
    use maho_tui::tui::Component;
    let (mut mode, directory) = native_mode();
    std::fs::write(
        directory.path().join("keybindings.json"),
        r#"{"app.session.resume":"ctrl+k"}"#,
    )
    .expect("configuration");
    mode.handle_input_at("\x0b", 0);
    assert!(mode.render(80).join("\n").contains("Resume Session"));
}

#[test]
fn settings_theme_preview_swaps_the_live_theme_without_persisting_it() {
    let (mut mode, _directory, session) = native_mode_with_session_and_provider_retaining_session(
        None,
        |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
        "faux",
        "faux-1",
        false,
    );
    let before = session.with_settings_manager(|settings| settings.get_string("theme"));
    mode.handle_ui_request(
        maho_interactive::interactive_extension_ui::UiRequest::ThemePreview("light".into()),
    );
    assert_eq!(mode.active_theme().name, "light");
    assert_eq!(
        session.with_settings_manager(|settings| settings.get_string("theme")),
        before
    );
}

#[tokio::test]
async fn theme_switch_recolors_existing_transcript_and_keeps_the_status_line() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("before-switch", Default::default())
        .await
        .expect("turn");
    mode.set_tools_expanded(true);
    let light =
        maho_interactive::theme::Theme::builtin("light", mode.active_theme().get_color_mode())
            .expect("light theme");
    let light_bg = light.get_bg_ansi(maho_interactive::theme::ThemeBg::UserMessageBg);
    let dark_bg = mode
        .active_theme()
        .get_bg_ansi(maho_interactive::theme::ThemeBg::UserMessageBg);
    assert_ne!(
        light_bg, dark_bg,
        "the builtin themes differ in the user message background"
    );
    let rendered = mode.render(80).join("\n");
    assert!(
        rendered.contains(&dark_bg) && !rendered.contains(&light_bg),
        "{rendered:?}"
    );
    mode.handle_ui_request(
        maho_interactive::interactive_extension_ui::UiRequest::SettingChanged(
            "theme".into(),
            serde_json::json!("light"),
        ),
    );
    let rendered = mode.render(80).join("\n");
    assert!(rendered.contains("before-switch"), "{rendered}");
    assert!(
        rendered.contains(&light_bg),
        "the message rendered before the switch uses the new theme: {rendered:?}"
    );
    assert!(
        !rendered.contains(&dark_bg),
        "no stale dark background remains: {rendered:?}"
    );
    assert!(
        rendered.contains("Tool output: expanded"),
        "the status line survives the switch: {rendered}"
    );
}

#[test]
fn unloadable_theme_setting_falls_back_to_dark_and_shows_an_error() {
    use maho_tui::tui::Component;
    let (mut mode, _directory, _session) = native_mode_with_session_and_provider_retaining_session(
        None,
        |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
        "faux",
        "faux-1",
        false,
    );
    mode.handle_ui_request(
        maho_interactive::interactive_extension_ui::UiRequest::SettingChanged(
            "theme".into(),
            serde_json::json!("no-such-theme"),
        ),
    );
    assert_eq!(mode.active_theme().name, "dark");
    assert!(
        mode.render(80)
            .join("\n")
            .contains("Failed to load theme \"no-such-theme\"")
    );
}

#[test]
fn tui_mode_change_is_saved_only_when_the_host_accepts_it() {
    use maho_interactive::tui_renderer::TuiMode;
    use maho_tui::tui::Component;
    let (mut mode, _directory, session) = native_mode_with_session_and_provider_retaining_session(
        None,
        |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
        "faux",
        "faux-1",
        false,
    );
    let request = || {
        maho_interactive::interactive_extension_ui::UiRequest::TerminalSetting(
            "tuiMode".into(),
            serde_json::json!("fullscreen"),
        )
    };
    mode.handle_ui_request(request());
    assert_eq!(
        mode.pending_terminal_setting(TuiMode::Regular)
            .map(|(next, _)| next),
        Some(TuiMode::Fullscreen)
    );
    mode.reject_terminal_setting(TuiMode::Regular);
    assert_eq!(
        session.with_settings_manager(|settings| settings.get_string("tuiMode")),
        None
    );
    assert!(
        mode.render(80)
            .join("\n")
            .contains("Close active overlays before changing TUI mode")
    );
    mode.handle_ui_request(request());
    assert!(mode.commit_terminal_setting());
    assert_eq!(
        session
            .with_settings_manager(|settings| settings.get_string("tuiMode"))
            .as_deref(),
        Some("fullscreen")
    );
    assert!(mode.render(80).join("\n").contains("TUI mode: fullscreen"));
}

/// Poll `fut` exactly once with a noop waker. A `Pending` result means the UI entry awaited slow work.
fn poll_once<T>(
    fut: &mut std::pin::Pin<&mut impl std::future::Future<Output = T>>,
) -> std::task::Poll<T> {
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    fut.as_mut().poll(&mut context)
}

/// Host whose prompt, set_model, and cycle_model pend forever once polled. `entered` flips only when
/// that future is actually polled, so a UI entry that parks the call leaves it false.
struct NeverReadyHost {
    entered: std::sync::Arc<std::sync::atomic::AtomicBool>,
    polls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    models: Vec<serde_json::Value>,
    listener: std::sync::Arc<
        std::sync::Mutex<Option<maho_interactive::interactive_host_runtime::SessionEventListener>>,
    >,
}

impl maho_interactive::interactive_session::InteractiveSession for NeverReadyHost {
    fn session_id(&self) -> Option<String> {
        None
    }
    fn session_file(&self) -> Option<String> {
        None
    }
    fn cwd(&self) -> String {
        String::new()
    }
    fn is_reconnecting(&self) -> bool {
        false
    }
    fn is_fallback(&self) -> bool {
        false
    }
    fn remote_state(
        &self,
    ) -> Option<maho_interactive::interactive_host_runtime::RemoteSessionState> {
        None
    }
    fn subscribe_session_events(
        &self,
        listener: maho_interactive::interactive_host_runtime::SessionEventListener,
    ) -> Option<maho_interactive::interactive_host_runtime::SessionSubscription> {
        *self
            .listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(listener);
        None
    }
    fn request_remote_history(
        &self,
        _generation: u64,
        _sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>,
    ) {
    }
    fn request_remote_models(
        &self,
        generation: u64,
        sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>,
    ) {
        let _ = sender.send((generation, Ok(self.models.clone())));
    }
    fn prompt(
        &self,
        _message: String,
        _options: maho_core::agent_session::PromptOptions,
    ) -> maho_interactive::interactive_session::SessionFuture<'_, Result<(), String>> {
        let entered = self.entered.clone();
        let polls = self.polls.clone();
        Box::pin(async move {
            entered.store(true, std::sync::atomic::Ordering::SeqCst);
            polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::future::pending::<()>().await;
            Ok(())
        })
    }
    fn set_model(
        &self,
        _provider: String,
        _id: String,
    ) -> maho_interactive::interactive_session::SessionFuture<'_, Result<(), String>> {
        let entered = self.entered.clone();
        Box::pin(async move {
            entered.store(true, std::sync::atomic::Ordering::SeqCst);
            std::future::pending::<()>().await;
            Ok(())
        })
    }
    fn cycle_model(
        &self,
        _forward: bool,
    ) -> maho_interactive::interactive_session::SessionFuture<'_, Result<Option<String>, String>>
    {
        let entered = self.entered.clone();
        Box::pin(async move {
            entered.store(true, std::sync::atomic::Ordering::SeqCst);
            std::future::pending::<()>().await;
            Ok(Some("cycled".into()))
        })
    }
    fn new_session(
        &self,
        _parent_session: Option<String>,
    ) -> maho_interactive::interactive_session::SessionFuture<
        '_,
        Result<maho_interactive::interactive_session::ReplacementOutcome, String>,
    > {
        Box::pin(async { Ok(maho_interactive::interactive_session::ReplacementOutcome::Cancelled) })
    }
    fn switch_session(
        &self,
        _session_path: String,
    ) -> maho_interactive::interactive_session::SessionFuture<
        '_,
        Result<maho_interactive::interactive_session::ReplacementOutcome, String>,
    > {
        Box::pin(async { Ok(maho_interactive::interactive_session::ReplacementOutcome::Cancelled) })
    }
    fn fork(
        &self,
        _entry_id: String,
        _include_entry: bool,
    ) -> maho_interactive::interactive_session::SessionFuture<
        '_,
        Result<maho_interactive::interactive_session::ForkOutcome, String>,
    > {
        Box::pin(async {
            Ok(maho_interactive::interactive_session::ForkOutcome {
                outcome: maho_interactive::interactive_session::ReplacementOutcome::Cancelled,
                editor_text: None,
            })
        })
    }
    fn execute_bash(
        &self,
        _command: String,
        _exclude_from_context: bool,
    ) -> maho_interactive::interactive_session::SessionFuture<'_, Result<serde_json::Value, String>>
    {
        let entered = self.entered.clone();
        Box::pin(async move {
            entered.store(true, std::sync::atomic::Ordering::SeqCst);
            std::future::pending::<()>().await;
            Ok(serde_json::json!({"exitCode": 0}))
        })
    }
    fn navigate_tree(
        &self,
        _entry_id: String,
        _options: maho_core::agent_session::TreeNavigationOptions,
    ) -> maho_interactive::interactive_session::SessionFuture<
        '_,
        Result<maho_core::agent_session::AssistantEditResult, String>,
    > {
        let entered = self.entered.clone();
        Box::pin(async move {
            entered.store(true, std::sync::atomic::Ordering::SeqCst);
            std::future::pending::<()>().await;
            Ok(maho_core::agent_session::AssistantEditResult::default())
        })
    }
    fn dispose(&self) -> maho_interactive::interactive_session::SessionFuture<'_, ()> {
        Box::pin(async {})
    }
}

fn never_ready_mode() -> (
    maho_interactive::interactive_mode::InteractiveMode,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    tempfile::TempDir,
) {
    let (mode, directory, session) = native_mode_with_session_and_provider_retaining_session(
        None,
        |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
        "faux",
        "faux-1",
        false,
    );
    let entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let polls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let model = serde_json::to_value(session.model()).expect("model json");
    let mut mode = mode;
    mode.set_session_host(std::sync::Arc::new(NeverReadyHost {
        entered: entered.clone(),
        polls: polls.clone(),
        models: vec![model],
        listener: std::sync::Arc::new(std::sync::Mutex::new(None)),
    }));
    mode.drain_events();
    (mode, entered, polls, directory)
}

#[tokio::test]
#[allow(clippy::unused_async)]
async fn r2_new_turn_paints_echo_and_working_before_the_first_poll() {
    use maho_tui::tui::Component;
    let (mut mode, entered, _polls, _directory) = never_ready_mode();
    mode.enqueue_submission("echo-me", None);
    let first = {
        let mut step = std::pin::pin!(mode.pump_turn_with(std::task::Waker::noop()));
        poll_once(&mut step)
    };
    assert!(
        first.is_ready(),
        "pump_turn must not poll the new turn before paint"
    );
    assert!(
        !entered.load(std::sync::atomic::Ordering::SeqCst),
        "the prompt future is still parked"
    );
    let frame = mode.render(80).join("\n");
    assert!(
        frame.contains("echo-me"),
        "the user echo paints before the first poll: {frame}"
    );
    assert!(
        mode.working_frame(0.0).is_some(),
        "the working row paints before the first poll"
    );
}

#[tokio::test]
#[allow(clippy::unused_async)]
async fn r5_model_confirm_paints_provisional_status_before_set_model() {
    use maho_tui::tui::Component;
    let (mut mode, entered, _polls, _directory) = never_ready_mode();
    mode.enqueue_submission(
        &maho_interactive::interactive_mode::internal_command("/model-select faux/faux-1"),
        None,
    );
    let first = {
        let mut step = std::pin::pin!(mode.dispatch_submission());
        poll_once(&mut step)
    };
    assert!(first.is_ready(), "model confirm must not await set_model");
    assert!(
        !entered.load(std::sync::atomic::Ordering::SeqCst),
        "set_model is parked"
    );
    let frame = mode.render(80).join("\n");
    assert!(
        frame.contains("Model: faux-1"),
        "provisional Model status paints before set_model: {frame}"
    );
}

#[tokio::test]
#[allow(clippy::unused_async)]
async fn r5b_cycle_key_paints_provisional_status_before_cycle() {
    use maho_tui::tui::Component;
    let (mut mode, entered, _polls, _directory) = never_ready_mode();
    let first = {
        let mut step = std::pin::pin!(mode.handle_filtered_runtime_input("\x10", 0));
        poll_once(&mut step)
    };
    assert!(first.is_ready(), "cycle must not await host_cycle_model");
    assert!(
        !entered.load(std::sync::atomic::Ordering::SeqCst),
        "cycle_model is parked"
    );
    let frame = mode.render(80).join("\n");
    assert!(
        frame.contains("Model:"),
        "provisional Model status paints before the cycle: {frame}"
    );
}

#[tokio::test]
#[allow(clippy::unused_async)]
async fn r8_follow_up_while_a_turn_runs_parks_the_enqueue() {
    let (mut mode, entered, polls, _directory) = never_ready_mode();
    mode.enqueue_submission("first", None);
    let first = {
        let mut step = std::pin::pin!(mode.dispatch_submission());
        poll_once(&mut step)
    };
    assert!(first.is_ready(), "starting the turn does not await it");
    assert!(
        mode.has_pending_turn(),
        "the first prompt is parked as the turn"
    );
    mode.enqueue_submission("second", None);
    let second = {
        let mut step = std::pin::pin!(mode.dispatch_submission());
        poll_once(&mut step)
    };
    assert!(second.is_ready(), "a second Enter must not await prompt");
    mode.enqueue_submission("third", None);
    let third = {
        let mut step = std::pin::pin!(mode.dispatch_submission());
        poll_once(&mut step)
    };
    assert!(
        third.is_ready(),
        "a later Enter must not await prompt either"
    );
    assert!(
        !entered.load(std::sync::atomic::Ordering::SeqCst),
        "neither prompt was polled before paint"
    );
    assert_eq!(
        polls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "nothing is polled before paint"
    );
    mode.poll_after_paint(std::task::Waker::noop());
    // The in-flight turn and the first parked enqueue are polled; the later enqueue waits.
    assert_eq!(
        polls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "park order: the later enqueue is not polled while the earlier one is pending"
    );
}

#[tokio::test]
async fn r4_default_model_write_is_not_on_the_ui_task_before_paint() {
    let (mut mode, _directory, session) = native_mode_with_session_and_provider_retaining_session(
        None,
        |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
        "faux",
        "faux-1",
        false,
    );
    // The selector lists only models with configured auth; without a key it is empty and Enter selects nothing.
    session
        .model_registry()
        .auth_storage
        .set_runtime_api_key("faux", "test-key");
    let open = {
        let mut step = std::pin::pin!(mode.handle_filtered_runtime_input("\x0c", 0));
        poll_once(&mut step)
    };
    assert!(
        open.is_ready(),
        "opening the selector does not write settings"
    );
    let confirm = {
        let mut step = std::pin::pin!(mode.handle_filtered_runtime_input("\r", 1));
        poll_once(&mut step)
    };
    assert!(
        confirm.is_ready(),
        "confirm must return before the settings write"
    );
    let written = session.with_settings_manager(|settings| settings.get_string("defaultModel"));
    assert!(
        written.is_none(),
        "defaultModel is not written before paint: {written:?}"
    );
    let (wake_tx, mut wake) = tokio::sync::mpsc::unbounded_channel();
    let waker = std::task::Waker::from(std::sync::Arc::new(TurnWaker { wake: wake_tx }));
    let mut wakes = 0usize;
    loop {
        mode.poll_after_paint(&waker);
        let model = session.with_settings_manager(|settings| settings.get_string("defaultModel"));
        if model.as_deref() == Some("faux-1") {
            break;
        }
        wakes += 1;
        assert!(wakes < 8, "the parked settings write did not settle");
        tokio::time::timeout(std::time::Duration::from_secs(10), wake.recv())
            .await
            .expect("the parked settings write must wake the frame within 10s")
            .expect("the parked settings write wakes the frame");
    }
    assert_eq!(
        session
            .with_settings_manager(|settings| settings.get_string("defaultProvider"))
            .as_deref(),
        Some("faux")
    );
    assert_eq!(
        session
            .with_settings_manager(|settings| settings.get_string("defaultModel"))
            .as_deref(),
        Some("faux-1")
    );
}

#[tokio::test]
async fn r1_matching_renderer_revision_does_not_await_the_snapshot() {
    let (mut mode, _directory, session) = native_mode_with_session_and_provider_retaining_session(
        None,
        |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
        "faux",
        "faux-1",
        false,
    );
    mode.enqueue_submission("/rename kept-name", None);
    let install = {
        let mut step = std::pin::pin!(mode.dispatch_submission());
        poll_once(&mut step)
    };
    assert!(
        install.is_ready(),
        "submitting parks the snapshot instead of awaiting it"
    );
    mode.poll_after_paint(std::task::Waker::noop());
    assert!(
        !mode.has_parked_action_for_test(),
        "the uncontended snapshot settles after paint"
    );
    // Hold the real runner lock. The old path awaited `tool_renderers_snapshot` (the runner lock) on
    // every submission, so it would be Pending here; a matching revision must not touch it.
    let guard = session.hold_extension_runner_lock_for_test().await;
    mode.enqueue_submission("hi-again", None);
    let again = {
        let mut step = std::pin::pin!(mode.dispatch_submission());
        poll_once(&mut step)
    };
    assert!(
        again.is_ready(),
        "a matching revision must not await the runner-lock snapshot"
    );
    assert!(
        !mode.has_parked_action_for_test(),
        "a matching revision does not park another snapshot"
    );
    let mut snapshot = std::pin::pin!(session.tool_renderers_snapshot());
    assert!(
        poll_once(&mut snapshot).is_pending(),
        "the real snapshot is Pending while the runner lock is held"
    );
    drop(guard);
}

/// Provider whose catalog refresh never resolves. `entered` flips only when `refresh_models` is polled.
struct StalledCatalog {
    entered: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl maho_ai::models::Provider for StalledCatalog {
    fn id(&self) -> &str {
        "stalled-catalog"
    }
    fn name(&self) -> &str {
        "stalled-catalog"
    }
    fn get_models(&self) -> Vec<maho_ai::types::Model> {
        Vec::new()
    }
    fn supports_refresh(&self) -> bool {
        true
    }
    fn refresh_models<'a>(
        &'a self,
        _context: maho_ai::models::RefreshModelsContext,
    ) -> maho_ai::types::BoxFuture<'a, Result<(), maho_ai::models::ModelsError>> {
        let entered = self.entered.clone();
        Box::pin(async move {
            entered.store(true, std::sync::atomic::Ordering::SeqCst);
            std::future::pending::<()>().await;
            Ok(())
        })
    }
    fn stream(
        &self,
        _model: &maho_ai::types::Model,
        _context: &maho_ai::types::Context,
        _options: Option<maho_ai::types::StreamOptions>,
    ) -> maho_ai::types::AssistantMessageEventStream {
        maho_ai::types::AssistantMessageEventStream::assistant()
    }
    fn stream_simple(
        &self,
        _model: &maho_ai::types::Model,
        _context: &maho_ai::types::Context,
        _options: Option<maho_ai::types::SimpleStreamOptions>,
    ) -> maho_ai::types::AssistantMessageEventStream {
        maho_ai::types::AssistantMessageEventStream::assistant()
    }
}

fn mode_with_stalled_catalog(
    entered: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> (
    maho_interactive::interactive_mode::InteractiveMode,
    tempfile::TempDir,
) {
    use maho_ai::models::Provider;
    use maho_ai::providers::faux::{
        FauxAssistantMessageOptions, FauxModelDefinition, RegisterFauxProviderOptions,
        faux_assistant_message, faux_provider, faux_streams,
    };
    use maho_core::agent_session::{AgentSession, AgentSessionConfig};
    use std::sync::Arc;
    let directory = tempfile::tempdir().expect("directory");
    let cwd = directory.path().to_string_lossy().into_owned();
    let provider = faux_provider(RegisterFauxProviderOptions {
        api: Some("faux".into()),
        provider: Some("faux".into()),
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".into(),
            ..Default::default()
        }]),
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("valid faux provider");
    let model = provider.get_model(Some("faux-1")).expect("model");
    provider.set_responses(vec![
        faux_assistant_message(
            "hello",
            FauxAssistantMessageOptions {
                timestamp: Some(0),
                ..Default::default()
            },
        )
        .into(),
    ]);
    let streams = faux_streams(provider.core.clone());
    let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| {
        let mut context = context.clone();
        context.system_prompt = context.system_prompt.filter(|prompt| !prompt.is_empty());
        streams.stream_simple(model, &context, options.map(|options| options.simple))
    });
    let stalled: Arc<dyn Provider> = Arc::new(StalledCatalog { entered });
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(
        maho_core::model_runtime::CreateModelRuntimeOptions {
            models_path: Some(directory.path().join("models.json")),
            auth_path: Some(directory.path().join("auth.json")),
            providers: Some(vec![provider.provider.clone(), stalled]),
            ..Default::default()
        },
    )
    .expect("valid model runtime");
    let session = AgentSession::new(AgentSessionConfig {
        agent: maho_agent::Agent::new(maho_agent::AgentOptions {
            initial_state: Some(maho_agent::agent::PartialAgentState {
                model: Some(model),
                ..Default::default()
            }),
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
        session_manager: maho_core::session_manager::SessionManager::in_memory(&cwd, None, None),
        settings_manager: maho_core::settings_manager::SettingsManager::from_storage(
            Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),
            false,
        ),
        cwd: cwd.clone(),
        agent_dir: Some(cwd),
        fallback_now: Some(Arc::new(|| 0.0)),
        retry_random: Some(Arc::new(|| 0.5)),
        scoped_models: Vec::new(),
        favorite_models: Vec::new(),
        flag_values: Default::default(),
        custom_tools: Vec::new(),
        model_runtime: Some(runtime),
        model_registry: None,
        uses_default_stream_function: Some(false),
        initial_active_tool_names: None,
        default_tool_names: None,
        eval_only_tool_names: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        session_start_event: None,
        auto_title_sessions: Some(false),
    })
    .expect("session");
    let mode = maho_interactive::interactive_mode::InteractiveMode::new(
        Arc::new(session),
        maho_interactive::theme::Theme::builtin(
            "dark",
            maho_interactive::theme::ColorMode::Truecolor,
        )
        .expect("theme"),
        std::rc::Rc::new(EditorHost),
    );
    (mode, directory)
}

#[tokio::test]
#[allow(clippy::unused_async)]
async fn r6_model_cache_miss_paints_the_selector_before_catalog_refresh() {
    use maho_tui::tui::Component;
    let entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (mut mode, _directory) = mode_with_stalled_catalog(entered.clone());
    mode.enqueue_submission("/model no-such-model", None);
    let first = {
        let mut step = std::pin::pin!(mode.dispatch_submission());
        poll_once(&mut step)
    };
    assert!(
        first.is_ready(),
        "/model miss must not await the catalog refresh"
    );
    assert!(
        !entered.load(std::sync::atomic::Ordering::SeqCst),
        "the provider refresh has not been polled"
    );
    let rendered = mode.render(80).join("\n");
    assert!(
        rendered.contains("Refreshing model catalogs…"),
        "the refresh status paints immediately: {rendered}"
    );
    let plain = maho_tools::ansi::strip_ansi(&rendered);
    assert!(
        plain.contains("> no-such-model"),
        "the cached selector paints before the refresh: {plain}"
    );
}

fn poll_dispatch(
    mode: &mut maho_interactive::interactive_mode::InteractiveMode,
) -> std::task::Poll<Result<Option<maho_core::agent_session::PromptDisposition>, String>> {
    let mut step = std::pin::pin!(mode.dispatch_submission());
    poll_once(&mut step)
}

#[tokio::test]
#[allow(clippy::unused_async)]
async fn r9_new_session_parks_the_replacement() {
    let (mut mode, _directory, session) = native_mode_with_session_and_provider_retaining_session(
        None,
        |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
        "faux",
        "faux-1",
        false,
    );
    let before = session.session_id();
    mode.enqueue_submission("/new", None);
    assert!(
        poll_dispatch(&mut mode).is_ready(),
        "/new must not await new_session"
    );
    assert_eq!(session.session_id(), before, "the replacement has not run");
    assert!(
        mode.has_parked_action_for_test(),
        "new_session stays parked until after paint"
    );
}

#[tokio::test]
#[allow(clippy::unused_async)]
async fn r10_tree_navigate_parks_the_host_call() {
    let (mut mode, _directory, session) = native_mode_with_session_and_provider_retaining_session(
        None,
        |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
        "faux",
        "faux-1",
        false,
    );
    session
        .with_settings_manager_mut(|settings| {
            settings.set(
                maho_core::settings_manager::SettingsScope::Global,
                &[(
                    "branchSummary".into(),
                    serde_json::json!({"skipPrompt": true}),
                )]
                .into_iter()
                .collect(),
            )
        })
        .expect("skip prompt");
    let entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let model = serde_json::to_value(session.model()).expect("model json");
    mode.set_session_host(std::sync::Arc::new(NeverReadyHost {
        entered: entered.clone(),
        polls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        models: vec![model],
        listener: std::sync::Arc::new(std::sync::Mutex::new(None)),
    }));
    mode.drain_events();
    mode.enqueue_submission(
        &maho_interactive::interactive_mode::internal_command("/tree not-the-leaf"),
        None,
    );
    assert!(
        poll_dispatch(&mut mode).is_ready(),
        "tree navigate must return before navigate_tree"
    );
    assert!(
        !entered.load(std::sync::atomic::Ordering::SeqCst),
        "navigate_tree is parked, not polled"
    );
}

#[tokio::test]
#[allow(clippy::unused_async)]
async fn r11_paste_and_tree_copy_park_the_clipboard() {
    use maho_tui::tui::Component;
    // Clipboard helpers are free functions with no seam to substitute a pending future. The
    // regression is the side effect a8ac6196 has already applied when the dispatch poll returns:
    // an immediately-ready copy (OSC 52 on a headless or SSH session) has already shown its status,
    // and a paste that returned text has already landed in the editor.
    let (mut mode, _directory) = native_mode();
    mode.enqueue_submission(
        &maho_interactive::interactive_mode::internal_command("/paste-clipboard"),
        None,
    );
    assert!(
        poll_dispatch(&mut mode).is_ready(),
        "paste must not await the clipboard"
    );
    assert!(
        mode.editor.editor.get_text().is_empty(),
        "the paste is not applied before paint"
    );

    let (mut mode, _directory) = native_mode();
    mode.queue_tree_copy_for_test(Some("copied-row".into()));
    mode.enqueue_submission("after-copy", None);
    assert!(
        poll_dispatch(&mut mode).is_ready(),
        "tree-copy drain must not await the clipboard"
    );
    assert!(mode.has_parked_action_for_test(), "the copy stays parked");
    assert!(
        !mode.has_pending_turn(),
        "the queued submission waits until the copy settles"
    );
    let rendered = mode.render(80).join("\n");
    assert!(
        !rendered.contains("Copied selected message"),
        "the copy status is not applied before paint: {rendered}"
    );
    assert!(
        !rendered.contains("after-copy"),
        "the queued prompt has not started: {rendered}"
    );
}

#[tokio::test]
async fn pasted_image_path_attaches_the_image() {
    // A terminal that saves the clipboard bitmap to a file pastes its shell-quoted path.
    let (mut mode, directory) = native_mode();
    let image = directory.path().join("clip board.png");
    std::fs::write(
        &image,
        [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99,
            0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ],
    )
    .expect("write image");
    let pasted = format!("\x1b[200~'{}' \x1b[201~", image.display());
    mode.handle_runtime_input(&pasted, 0).await.expect("paste");
    assert!(mode.has_parked_action_for_test(), "the file read is parked");
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        std::future::poll_fn(|context| {
            mode.poll_after_paint(context.waker());
            if mode.has_parked_action_for_test() {
                std::task::Poll::Pending
            } else {
                std::task::Poll::Ready(())
            }
        }),
    )
    .await
    .expect("the parked read settles");
    assert_eq!(mode.editor.editor.get_text(), "[Image #1]");
}

#[tokio::test]
#[allow(clippy::unused_async)]
async fn r12_keybindings_routes_through_external_editor() {
    let (mut mode, directory) = native_mode();
    mode.arm_keybindings_editor_for_test("this-editor-must-not-launch".into());
    mode.enqueue_submission("/keybindings", None);
    assert!(
        poll_dispatch(&mut mode).is_ready(),
        "/keybindings must not await the editor"
    );
    assert_eq!(
        mode.take_terminal_request(),
        Some(maho_interactive::interactive_mode::TerminalRequest::ExternalEditor)
    );
    assert!(
        !directory.path().join("keybindings.json").exists(),
        "the editor is not launched before the terminal stops"
    );
}

#[tokio::test]
#[allow(clippy::unused_async)]
async fn r13_command_tails_park_before_paint() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.enqueue_submission("/reload", None);
    assert!(
        poll_dispatch(&mut mode).is_ready(),
        "/reload must not await the veto"
    );
    assert!(
        mode.render(120)
            .join("\n")
            .contains("Reloading keybindings"),
        "the reload box paints before the veto"
    );
    assert!(
        matches!(poll_dispatch(&mut mode), std::task::Poll::Ready(Ok(None))),
        "/reload-run is not queued until the veto settles"
    );

    let (mut mode, directory, session) = native_mode_with_session_and_provider_retaining_session(
        None,
        |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
        "faux",
        "faux-1",
        false,
    );
    let path = directory.path().join("out.jsonl");
    mode.enqueue_submission(&format!("/export {}", path.display()), None);
    assert!(
        poll_dispatch(&mut mode).is_ready(),
        "/export must not write before paint"
    );
    assert!(!path.exists(), "the export file is not written yet");
    assert!(mode.has_parked_action_for_test());

    mode.enqueue_submission("/rename parked-name", None);
    assert!(
        poll_dispatch(&mut mode).is_ready(),
        "/rename must not await set_session_name"
    );
    assert!(
        session.session_name().is_none(),
        "the name is not set before paint"
    );

    // `list_credentials` returns immediately. a8ac6196 has already shown the empty-list status
    // when the dispatch poll returns; the parked list has not.
    let (mut mode, _directory) = native_mode();
    mode.enqueue_submission("/logout", None);
    assert!(
        poll_dispatch(&mut mode).is_ready(),
        "/logout must not await the credential list"
    );
    let logged_out = mode.render(80).join("\n");
    assert!(
        !logged_out.contains("No stored credentials"),
        "the list result is not applied yet: {logged_out}"
    );
    assert!(mode.has_parked_action_for_test());

    // `gh auth status` cannot be substituted. If it is still running, a8ac6196's dispatch poll is
    // Pending; if it has already returned, the auth or gist line is already on screen.
    let (mut mode, _directory) = native_mode();
    mode.enqueue_submission("/share", None);
    assert!(
        poll_dispatch(&mut mode).is_ready(),
        "/share must not await gh auth"
    );
    let rendered = mode.render(80).join("\n");
    assert!(
        !rendered.contains("Creating gist"),
        "the loader waits for auth and export: {rendered}"
    );
    assert!(
        !rendered.contains("GitHub CLI"),
        "auth has not finished: {rendered}"
    );
    assert!(mode.has_parked_action_for_test());

    let (mut mode, entered, _polls, _directory) = never_ready_mode();
    mode.enqueue_submission("!echo parked", None);
    assert!(
        poll_dispatch(&mut mode).is_ready(),
        "host ! must not await execute_bash"
    );
    assert!(
        !entered.load(std::sync::atomic::Ordering::SeqCst),
        "execute_bash is parked"
    );
}

#[tokio::test]
#[allow(clippy::unused_async)]
async fn r16_tree_and_fork_open_mount_on_a_later_frame() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.enqueue_submission("/tree", None);
    assert!(
        poll_dispatch(&mut mode).is_ready(),
        "/tree must not walk the session on the UI task"
    );
    let rendered = mode.render(80).join("\n");
    assert!(
        !rendered.contains("Session Tree"),
        "the selector is not mounted yet: {rendered}"
    );
    assert!(
        !rendered.contains("No entries in session"),
        "the walk has not finished"
    );
    assert!(mode.has_parked_action_for_test());

    let (mut mode, _directory) = native_mode();
    mode.enqueue_submission("/fork", None);
    assert!(
        poll_dispatch(&mut mode).is_ready(),
        "/fork must not walk the session on the UI task"
    );
    let rendered = mode.render(80).join("\n");
    assert!(
        !rendered.contains("Fork from Message"),
        "the selector is not mounted yet: {rendered}"
    );
    assert!(!rendered.contains("No messages to fork from"));
    assert!(mode.has_parked_action_for_test());
}

#[tokio::test]
async fn consent_selector_mounts_before_login_dialog_and_cancel_skips_login() {
    use maho_tui::tui::Component;
    let (mut mode, directory) = native_mode_with_provider(None, "github-copilot", "faux-1", false);
    mode.enqueue_submission(&maho_interactive::interactive_mode::internal_command("/login-provider github-copilot api_key"), None);
    assert_eq!(mode.dispatch_submission().await.expect("login"), Some(maho_core::agent_session::PromptDisposition::Handled));
    let rendered = mode.render(100).join("\n");
    assert!(rendered.contains("Account risk"), "{rendered}");
    assert!(rendered.contains("I understand the risk - continue"), "{rendered}");
    assert!(!rendered.to_lowercase().contains("enter api key") && !rendered.contains("Paste"), "login dialog must not mount before consent: {rendered}");
    mode.enqueue_submission(&maho_interactive::interactive_mode::internal_command("/login-consent-cancel"), None);
    assert_eq!(mode.dispatch_submission().await.expect("cancel"), Some(maho_core::agent_session::PromptDisposition::Handled));
    assert!(!directory.path().join("settings.json").exists());
}

#[tokio::test]
async fn consent_continue_records_consent_then_queues_the_login() {
    use maho_tui::tui::Component;
    let (mut mode, _directory, session) = native_mode_with_session_and_provider_retaining_session(
        None, |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None), "github-copilot", "faux-1", false);
    assert!(!session.with_settings_manager(|s| maho_core::provider_consent::consent_recorded(s, "github-copilot")));
    mode.enqueue_submission(&maho_interactive::interactive_mode::internal_command("/login-consent github-copilot api_key"), None);
    assert_eq!(mode.dispatch_submission().await.expect("continue"), Some(maho_core::agent_session::PromptDisposition::Handled));
    assert!(session.with_settings_manager(|s| maho_core::provider_consent::consent_recorded(s, "github-copilot")));
    // The resumed `/login-provider` no longer shows the selector.
    assert_eq!(mode.dispatch_submission().await.expect("resume"), Some(maho_core::agent_session::PromptDisposition::Handled));
    assert!(!mode.render(100).join("\n").contains("Account risk"));
}
