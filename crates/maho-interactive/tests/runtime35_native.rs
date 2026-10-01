use maho_test_support::{faux::{FauxResponse, FauxScript}, faux_session::FauxSession};

struct EditorHost;
impl maho_tui::components::editor::EditorTuiHost for EditorHost {
    fn request_render(&self) {}
    fn terminal_rows(&self) -> usize { 36 }
}

fn native_mode() -> (maho_interactive::interactive_mode::InteractiveMode, tempfile::TempDir) {
    use std::sync::Arc;
    use maho_ai::providers::faux::{FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_provider, faux_streams};
    use maho_core::agent_session::{AgentSession, AgentSessionConfig};
    let directory = tempfile::tempdir().expect("directory");
    let cwd = directory.path().to_string_lossy().into_owned();
    let provider = faux_provider(RegisterFauxProviderOptions { api: Some("faux".into()), tokens_per_second: Some(0.0), ..Default::default() });
    let model = provider.get_model(Some("faux-1")).expect("model");
    provider.set_responses(vec![faux_assistant_message("hello", FauxAssistantMessageOptions { timestamp: Some(0), ..Default::default() }).into()]);
    let streams = faux_streams(provider.core.clone());
    let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| streams.stream_simple(model, context, options.map(|options| options.simple)));
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(directory.path().join("models.json")), auth_path: Some(directory.path().join("auth.json")),
        providers: Some(vec![provider.provider.clone()]), ..Default::default()
    });
    let session = AgentSession::new(AgentSessionConfig {
        agent: maho_agent::Agent::new(maho_agent::AgentOptions {
            initial_state: Some(maho_agent::agent::PartialAgentState { model: Some(model), ..Default::default() }),
            stream_fn: Some(stream_fn), ..Default::default()
        }),
        session_manager: maho_core::session_manager::SessionManager::in_memory(&cwd, None, None),
        settings_manager: maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()), false),
        cwd: cwd.clone(), agent_dir: Some(cwd), fallback_now: Some(Arc::new(|| 0.0)), retry_random: Some(Arc::new(|| 0.5)),
        scoped_models: Vec::new(), favorite_models: Vec::new(), flag_values: Default::default(), custom_tools: Vec::new(),
        model_runtime: Some(runtime), model_registry: None, uses_default_stream_function: Some(false), initial_active_tool_names: None,
        default_tool_names: None, eval_only_tool_names: None, allowed_tool_names: None, excluded_tool_names: None, base_tools_override: None,
        session_start_event: None, auto_title_sessions: Some(false),
    }).expect("session");
    let mode = maho_interactive::interactive_mode::InteractiveMode::new(Arc::new(session), maho_interactive::theme::Theme::builtin("dark", maho_interactive::theme::ColorMode::Truecolor).expect("theme"), std::rc::Rc::new(EditorHost));
    (mode, directory)
}

#[tokio::test]
async fn native_prompt_renders_one_user_card_and_one_completed_assistant() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(10), mode.submit("hi", Default::default())).await.expect("bounded turn").expect("prompt"), maho_core::agent_session::PromptDisposition::Started);
    assert!(mode.agent_idle);
    for width in [40, 80, 120] {
        let text = mode.render(width).join("\n");
        assert_eq!(text.matches("hi").count(), 1, "user echo at {width}");
        assert_eq!(text.matches("hello").count(), 1, "assistant at {width}");
    }
}

#[tokio::test]
async fn native_faux_events_include_user_and_final_assistant_before_idle() {
    let session = FauxSession::new(FauxScript { name: "interactive-native".into(), prompt: "hi".into(), responses: vec![FauxResponse { content: "hello".into(), stop_reason: "stop".into() }] });
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native()).await.expect("bounded turn").expect("native session");
    let events: Vec<maho_agent::types::AgentEvent> = serde_json::from_value(result["events"].clone()).expect("native typed events");
    assert!(matches!(events.first(), Some(maho_agent::types::AgentEvent::AgentStart)));
    assert!(matches!(events.last(), Some(maho_agent::types::AgentEvent::AgentEnd { .. })));
    assert_eq!(events.iter().filter(|event| matches!(event, maho_agent::types::AgentEvent::MessageEnd { .. })).count(), 2);
}

#[test]
fn tool_lifecycle_updates_one_card_and_replaces_partial_result() {
    use maho_agent::types::AgentEvent;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.handle_event(&AgentEvent::ToolExecutionStart { tool_call_id: "call-1".into(), tool_name: "custom".into(), args: serde_json::json!({}) });
    mode.handle_event(&AgentEvent::ToolExecutionUpdate { tool_call_id: "call-1".into(), tool_name: "custom".into(), args: serde_json::json!({}), partial_result: serde_json::json!({"content":[{"type":"text","text":"partial-value"}]}) });
    assert!(mode.render(80).join("\n").contains("partial-value"));
    mode.handle_event(&AgentEvent::ToolExecutionEnd { tool_call_id: "call-1".into(), tool_name: "custom".into(), result: serde_json::json!({"content":[{"type":"text","text":"final-value"}]}), is_error: false });
    let rendered = mode.render(80).join("\n");
    assert!(!rendered.contains("partial-value"));
    assert_eq!(rendered.matches("final-value").count(), 1);
}

#[test]
fn tool_end_without_start_still_renders_final_result() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.handle_event(&maho_agent::types::AgentEvent::ToolExecutionEnd { tool_call_id: "orphan".into(), tool_name: "custom".into(), result: serde_json::json!({"content":[{"type":"text","text":"failed-result"}]}), is_error: true });
    assert!(mode.render(80).join("\n").contains("failed-result"));
}

#[tokio::test]
async fn enter_captures_editor_text_clears_draft_and_submits_native_prompt() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.handle_input("hi");
    mode.handle_input("\r");
    assert!(mode.editor.editor.get_text().is_empty());
    assert_eq!(mode.submit_editor().await.expect("submit"), Some(maho_core::agent_session::PromptDisposition::Started));
    assert!(mode.render(80).join("\n").contains("hello"));
    assert_eq!(mode.submit_editor().await.expect("empty submission queue"), None);
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
    assert_eq!(mode.submit("/rename named-session", Default::default()).await.expect("rename"), maho_core::agent_session::PromptDisposition::Handled);
    assert!(mode.render(80).join("\n").contains("named-session"));
    assert_eq!(mode.submit("hi", Default::default()).await.expect("native turn"), maho_core::agent_session::PromptDisposition::Started);
    assert!(mode.render(80).join("\n").contains("hello"));
}

#[tokio::test]
async fn invalid_thinking_command_does_not_become_provider_input() {
    let (mut mode, _directory) = native_mode();
    assert!(mode.submit("/thinking nonsense", Default::default()).await.is_err());
    assert!(mode.agent_idle);
    assert_eq!(mode.submit("hi", Default::default()).await.expect("native turn"), maho_core::agent_session::PromptDisposition::Started);
}

#[tokio::test]
async fn rename_without_argument_opens_real_input_and_commits_enter() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    assert_eq!(mode.submit("/rename", Default::default()).await.expect("dialog"), maho_core::agent_session::PromptDisposition::Handled);
    mode.handle_input("dialog-name"); mode.handle_input("\r");
    assert!(mode.render(80).join("\n").contains("dialog-name"));
    assert!(mode.editor.editor.get_text().is_empty());
}

#[tokio::test]
async fn native_footer_reflects_session_name_and_selected_model() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("/name footer-name", Default::default()).await.expect("rename");
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
fn question_opens_shortcut_overlay_and_next_key_dismisses_without_typing() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let before = mode.render(80).len();
    mode.handle_input_at("?", 0);
    assert!(mode.render(80).len() > before);
    mode.handle_input_at("a", 1);
    assert!(mode.editor.editor.get_text().is_empty());
    assert_eq!(mode.render(80).len(), before);
}
