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

#[tokio::test]
async fn jsonl_export_preserves_native_messages_and_quoted_path() {
    let (mut mode, directory) = native_mode();
    mode.submit("hi", Default::default()).await.expect("prompt");
    let path = directory.path().join("export with spaces.jsonl");
    assert_eq!(mode.submit(&format!("/export \"{}\"", path.display()), Default::default()).await.expect("export"), maho_core::agent_session::PromptDisposition::Handled);
    let entries = std::fs::read_to_string(path).expect("export").lines().map(|line| serde_json::from_str::<serde_json::Value>(line).expect("jsonl")).collect::<Vec<_>>();
    assert_eq!(entries.iter().filter(|entry| entry["type"] == "message").count(), 2);
}

#[test]
fn command_path_parser_preserves_quotes_and_first_argument_semantics() {
    use maho_interactive::interactive_mode::get_path_command_argument;
    assert_eq!(get_path_command_argument("/export 'a b.jsonl' ignored", "/export").as_deref(), Some("a b.jsonl"));
    assert_eq!(get_path_command_argument("/export a.jsonl ignored", "/export").as_deref(), Some("a.jsonl"));
    assert_eq!(get_path_command_argument("/export 'unclosed", "/export"), None);
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
    mode.handle_input_at("\x1b[B", 0); mode.handle_input_at("\r", 1);
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(5), selected).await.expect("bounded selector"), Some("second".into()));
}

#[tokio::test]
async fn extension_input_uses_real_input_and_returns_typed_value() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let answer = ui.input("Name", None, Default::default());
    mode.render(80); mode.handle_input_at("typed-value", 0); mode.handle_input_at("\r", 1);
    assert_eq!(answer.await.as_deref(), Some("typed-value"));
}

#[test]
fn extension_editor_widgets_and_status_reach_native_surface() {
    use maho_ext_api::{ExtensionUi, WidgetContent};
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.extension_ui.set_editor_text("extension-draft");
    mode.extension_ui.set_widget("fixture", Some(WidgetContent::Lines(vec!["widget-value".into()])), Default::default());
    mode.extension_ui.set_status("fixture", Some("extension-status"));
    let lines = mode.render(120).join("\n");
    assert_eq!(mode.extension_ui.get_editor_text(), "extension-draft");
    assert!(lines.contains("widget-value")); assert!(lines.contains("extension-status"));
    mode.extension_ui.set_widget("fixture", None, Default::default());
    assert!(!mode.render(120).join("\n").contains("widget-value"));
}

#[test]
fn assistant_text_segments_remain_on_either_side_of_tool_card() {
    use maho_agent::types::AgentEvent;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let message: maho_agent::types::AgentMessage = serde_json::from_value(serde_json::json!({"role":"assistant", "content":[{"type":"text","text":"before-tool"},{"type":"toolCall","id":"ordered","name":"custom","arguments":{}},{"type":"text","text":"after-tool"}], "api":"faux", "provider":"faux", "model":"faux-1", "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},"stopReason":"toolUse","timestamp":0})).expect("message");
    mode.handle_event(&AgentEvent::MessageStart { message: message.clone() });
    mode.handle_event(&AgentEvent::MessageEnd { message });
    mode.handle_event(&AgentEvent::ToolExecutionEnd { tool_call_id:"ordered".into(), tool_name:"custom".into(), result:serde_json::json!({"content":[{"type":"text","text":"tool-output"}]}), is_error:false });
    let lines = mode.render(80).join("\n");
    assert!(lines.find("before-tool").expect("head") < lines.find("tool-output").expect("tool"));
    assert!(lines.find("tool-output").expect("tool") < lines.find("after-tool").expect("tail"));
    assert_eq!(lines.matches("after-tool").count(), 1);
}

#[tokio::test]
async fn registered_extension_markdown_transformer_reaches_native_assistant() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let mut api = maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("native-fixture", "/tmp".into(), Default::default()), Default::default(), Default::default(), Default::default());
    api.register_markdown_transformer(std::sync::Arc::new(|text, context| {
        if context.message_type == maho_ext_api::MarkdownMessageType::Assistant { text.replace("hello", "transformed-reply") } else { text.into() }
    }));
    mode.use_registered_markdown_transformers(&[api.registered]);
    mode.submit("hi", Default::default()).await.expect("native turn");
    let lines = mode.render(80).join("\n");
    assert!(lines.contains("transformed-reply")); assert!(!lines.contains("hello"));
}

#[tokio::test]
async fn rebuilding_native_history_does_not_duplicate_existing_transcript() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("hi", Default::default()).await.expect("turn");
    mode.rebuild_history(); mode.rebuild_history();
    assert_eq!(mode.render(80).join("\n").matches("hello").count(), 1);
}

#[test]
fn hidden_custom_history_is_not_rendered() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    for display in [false, true] {
        let message = maho_agent::types::AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(maho_agent::harness::messages::CustomMessage { role:"custom".into(), custom_type:"fixture".into(), content:maho_agent::harness::messages::CustomMessageContent::Text(if display {"visible-value"} else {"hidden-value"}.into()), display, details:None, timestamp:0 }));
        mode.add_history_message(&message);
    }
    let rendered = mode.render(80).join("\n");
    assert!(rendered.contains("visible-value")); assert!(!rendered.contains("hidden-value"));
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
    let result = FauxSession::new(FauxScript { name:"pacing".into(), prompt:"hi".into(), responses:vec![FauxResponse { content:"paced-reply".into(), stop_reason:"stop".into() }] }).run_native().await.expect("native");
    let message: maho_agent::types::AgentMessage = serde_json::from_value(result["messages"][1].clone()).expect("assistant");
    let assistant = message.as_assistant().expect("assistant").clone();
    let (mut mode, _directory) = native_mode();
    mode.handle_event(&maho_agent::types::AgentEvent::MessageStart { message:message.clone() });
    mode.handle_event(&maho_agent::types::AgentEvent::MessageUpdate { message:message.clone(), assistant_message_event:maho_ai::types::AssistantMessageEvent::TextDelta { content_index:0, delta:"paced-reply".into(), partial:assistant } });
    for frame in 100..200 { mode.tick(f64::from(frame) * 100.0); }
    assert!(mode.render(80).join("\n").contains("paced-reply"));
    mode.handle_event(&maho_agent::types::AgentEvent::MessageEnd { message });
    assert_eq!(mode.render(80).join("\n").matches("paced-reply").count(), 1);
}

#[tokio::test]
async fn session_command_reports_native_turn_without_starting_another() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("hi", Default::default()).await.expect("turn");
    assert_eq!(mode.submit("/session", Default::default()).await.expect("stats"), maho_core::agent_session::PromptDisposition::Handled);
    assert!(mode.render(80).join("\n").contains("In-memory"));
    assert!(mode.agent_idle);
}

#[test]
fn consecutive_extension_notifications_replace_status_not_append_it() {
    use maho_ext_api::{ExtensionUi, NotificationType};
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.extension_ui.notify("first-status", NotificationType::Info); mode.render(80);
    mode.extension_ui.notify("second-status", NotificationType::Info);
    let lines = mode.render(80).join("\n");
    assert!(!lines.contains("first-status")); assert_eq!(lines.matches("second-status").count(), 1);
}

#[test]
fn widget_refresh_keeps_insertion_order_and_caps_lines() {
    use maho_ext_api::{ExtensionUi, WidgetContent}; use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.extension_ui.set_widget("z", Some(WidgetContent::Lines(vec!["first-widget".into()])), Default::default());
    mode.extension_ui.set_widget("a", Some(WidgetContent::Lines(vec!["second-widget".into()])), Default::default());
    mode.render(80);
    mode.extension_ui.set_widget("z", Some(WidgetContent::Lines((0..12).map(|n| format!("updated-widget-{n}" )).collect())), Default::default());
    let lines = mode.render(80).join("\n");
    assert!(lines.find("updated-widget-0").expect("first") < lines.find("second-widget").expect("second"));
    assert!(!lines.contains("updated-widget-11"));
}
