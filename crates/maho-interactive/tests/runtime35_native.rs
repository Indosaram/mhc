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

#[test]
fn streamed_tool_arguments_finish_with_exact_parsed_arguments() {
    use maho_agent::types::AgentEvent;
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let message: maho_agent::types::AgentMessage = serde_json::from_value(serde_json::json!({"role":"assistant", "content":[{"type":"toolCall","id":"arguments","name":"custom","arguments":{"value":"exact-final"}}], "api":"faux", "provider":"faux", "model":"faux-1", "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},"stopReason":"toolUse","timestamp":0})).expect("message");
    mode.handle_event(&AgentEvent::AgentStart);
    mode.handle_event(&AgentEvent::MessageStart { message:message.clone() });
    mode.handle_event(&AgentEvent::MessageUpdate { message:message.clone(), assistant_message_event:maho_ai::types::AssistantMessageEvent::ToolcallDelta { content_index:0, delta:"{\"value\":\"partial".into(), partial:message.as_assistant().expect("assistant").clone() } });
    mode.handle_event(&AgentEvent::MessageEnd { message });
    mode.tick(1000.0);
    assert!(mode.render(80).join("\n").contains("exact-final"));
}

#[test]
fn plain_string_user_content_renders_in_native_transcript() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let message = serde_json::from_value(serde_json::json!({"role":"user","content":"string-user-message","timestamp":0})).expect("user message");
    mode.handle_event(&maho_agent::types::AgentEvent::MessageStart { message });
    assert!(mode.render(80).join("\n").contains("string-user-message"));
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

#[tokio::test]
async fn cancelled_extension_input_releases_composer() {
    use maho_ext_api::ExtensionUi; use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let ui = mode.extension_ui.clone();
    let signal = maho_tools::definition::AbortSignal::default();
    let answer = ui.input("Cancelled", None, maho_ext_api::ExtensionUiDialogOptions { signal:Some(signal.clone()), ..Default::default() });
    mode.render(80); signal.abort(); assert_eq!(answer.await, None);
    mode.render(80); mode.handle_input_at("new-draft", 0);
    assert_eq!(mode.editor.editor.get_text(), "new-draft");
}

#[test]
fn expansion_updates_completed_tool_cards_not_just_pending_cards() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let text = (0..100).map(|n| format!("line-{n}")).collect::<Vec<_>>().join("\n");
    mode.handle_event(&maho_agent::types::AgentEvent::ToolExecutionEnd { tool_call_id:"expanded".into(), tool_name:"ls".into(), result:serde_json::json!({"content":[{"type":"text","text":text}]}), is_error:false });
    assert!(!mode.render(80).join("\n").contains("line-99"));
    mode.set_tools_expanded(true);
    assert!(mode.render(80).join("\n").contains("line-99"));
}

#[tokio::test]
async fn bare_thinking_selector_filters_then_cancels_without_provider_turn() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("/thinking", Default::default()).await.expect("selector");
    mode.render(80); mode.handle_input_at("off", 0);
    mode.render(80); mode.handle_input_at("\x1b", 1);
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
    mode.handle_event(&maho_agent::types::AgentEvent::AgentEnd { messages:Vec::new() });
    assert!(mode.working_frame(800.0).is_none());
}

#[test]
fn extension_working_visibility_controls_native_active_row() {
    use maho_ext_api::ExtensionUi;
    let (mut mode, _directory) = native_mode();
    mode.handle_event(&maho_agent::types::AgentEvent::AgentStart);
    mode.extension_ui.set_working_visible(false).expect("hide"); mode.drain_events();
    assert!(mode.working_frame(100.0).is_none());
    mode.extension_ui.set_working_visible(true).expect("show"); mode.drain_events();
    assert!(mode.working_frame(100.0).is_some());
}

#[tokio::test]
async fn extension_editor_returns_prefill_through_real_multiline_component() {
    use maho_ext_api::ExtensionUi; use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode(); let ui = mode.extension_ui.clone();
    let result = ui.editor("Edit", Some("prefill")); mode.render(80);
    mode.handle_input_at("\r", 0);
    assert_eq!(result.await.expect("editor"), Some("prefill".into()));
}

#[test]
fn extension_terminal_input_transforms_consumes_and_unsubscribes() {
    use maho_ext_api::{ExtensionUi, TerminalInputResult};
    let (mut mode, _directory) = native_mode();
    let unsubscribe = mode.extension_ui.on_terminal_input(std::sync::Arc::new(|data| Some(TerminalInputResult { consume:Some(data == "blocked"), data:Some("replacement".into()) }))).expect("listener");
    mode.handle_input_at("blocked", 0); assert!(mode.editor.editor.get_text().is_empty());
    mode.handle_input_at("original", 1); assert_eq!(mode.editor.editor.get_text(), "replacement");
    unsubscribe(); mode.editor.editor.set_text(""); mode.handle_input_at("original", 2);
    assert_eq!(mode.editor.editor.get_text(), "original");
}

#[tokio::test]
async fn unintegrated_builtin_does_not_silently_become_provider_input() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    assert_eq!(mode.submit("/compact", Default::default()).await.expect("compact is handled with the pinned warning"), maho_core::agent_session::PromptDisposition::Handled);
    assert!(mode.render(80).join("\n").contains("Nothing to compact (no messages yet)"));
    assert!(mode.submit("/export session.html", Default::default()).await.is_err(), "an unintegrated command errors instead of reaching the provider");
    assert_eq!(mode.footer_snapshot().context_tokens, Some(0.0), "no command became a provider turn");
}

#[test]
fn extension_expansion_and_keyboard_expansion_share_state() {
    use maho_ext_api::ExtensionUi;
    let (mut mode, _directory) = native_mode();
    mode.extension_ui.set_tools_expanded(true).expect("expand"); mode.drain_events();
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
    assert!(lines.contains("request-1")); assert!(lines.contains("(no answer)"));
    assert!(!lines.contains("[Answer to question"));
}

#[tokio::test]
async fn new_command_replaces_native_session_and_clears_transcript() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("old-message", Default::default()).await.expect("turn");
    mode.submit("/new", Default::default()).await.expect("new");
    assert!(!mode.render(80).join("\n").contains("old-message"));
    assert_eq!(mode.footer_snapshot().context_tokens, Some(0.0));
}

#[tokio::test]
async fn empty_compact_shows_the_pinned_no_messages_warning() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    assert_eq!(mode.submit("/compact", Default::default()).await.expect("handled"), maho_core::agent_session::PromptDisposition::Handled);
    assert!(mode.render(80).join("\n").contains("Nothing to compact (no messages yet)"), "the pinned interactive warning replaces the core error");
    assert_eq!(mode.footer_snapshot().context_tokens, Some(0.0));
}

#[tokio::test]
async fn native_shell_submission_renders_and_persists_without_model_turn() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("!printf shell-output", Default::default()).await.expect("shell");
    assert!(mode.render(80).join("\n").contains("shell-output"));
    mode.rebuild_history(); assert!(mode.render(80).join("\n").contains("shell-output"));
}

#[tokio::test]
async fn bare_model_selector_cancels_and_releases_native_composer() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("/model", Default::default()).await.expect("selector");
    mode.render(80); mode.handle_input_at("\x1b", 0); mode.handle_input_at("draft", 1);
    assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[tokio::test]
async fn resume_path_restores_exported_native_transcript() {
    use maho_tui::tui::Component;
    let (mut mode, directory) = native_mode();
    mode.submit("saved-message", Default::default()).await.expect("turn");
    let path = directory.path().join("resume.jsonl");
    mode.submit(&format!("/export {}", path.display()), Default::default()).await.expect("export");
    mode.submit("/new", Default::default()).await.expect("new");
    mode.submit(&format!("/resume {}", path.display()), Default::default()).await.expect("resume");
    assert!(mode.render(80).join("\n").contains("saved-message"));
}

#[tokio::test]
async fn native_runtime_model_keys_do_not_insert_control_input() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.handle_runtime_input("\x10", 0).await.expect("cycle");
    assert!(mode.editor.editor.get_text().is_empty());
    mode.handle_runtime_input("\x0c", 1).await.expect("selector"); mode.render(80);
    mode.handle_runtime_input("\x1b", 2).await.expect("cancel");
    mode.handle_runtime_input("draft", 3).await.expect("type");
    assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[test]
fn native_editor_completes_builtin_command_from_canonical_catalog() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.editor.editor.set_text("/thi"); mode.render(80); mode.handle_input_at("\t", 0);
    assert!(mode.editor.editor.is_showing_autocomplete());
    mode.handle_input_at("\t", 1);
    assert!(mode.editor.editor.get_text().starts_with("/thinking"));
}

#[tokio::test]
async fn runtime_input_listener_consumes_model_shortcut_before_dispatch() {
    use maho_ext_api::{ExtensionUi, TerminalInputResult}; use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let _unsubscribe = mode.extension_ui.on_terminal_input(std::sync::Arc::new(|_| Some(TerminalInputResult { consume:Some(true), data:None }))).expect("listener");
    mode.handle_runtime_input("\x0c", 0).await.expect("input");
    assert!(!mode.render(80).join("\n").contains("Select Model"));
}

#[test]
fn skill_invocation_replay_separates_collapsed_skill_and_user_request() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    let text = maho_core::skill_invocation::format_skill_invocation_prompt(&[maho_core::skill_invocation::SkillInvocationPromptSkill { name:"sample".into(), file_path:"/sample/SKILL.md".into(), base_dir:"/sample".into(), body:"private-body".into() }], Some("user-request"));
    let message = serde_json::from_value(serde_json::json!({"role":"user","content":[{"type":"text","text":text}],"timestamp":1})).expect("message");
    mode.add_history_message(&message);
    let lines = mode.render(80).join("\n");
    assert!(lines.contains("sample")); assert!(lines.contains("user-request")); assert!(!lines.contains("private-body"));
    mode.set_tools_expanded(true);
    assert!(mode.render(80).join("\n").contains("private-body"));
}

#[tokio::test]
async fn extension_question_cancel_releases_composer_and_preserves_unanswered_ids() {
    use maho_ext_api::ExtensionUi; use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode(); let ui = mode.extension_ui.clone();
    let request = maho_ext_api::QuestionRequest { request_id:"q".into(), questions:vec![maho_ext_api::Question { id:"item".into(), header:"Header".into(), question:"Choose".into(), options:vec![maho_ext_api::QuestionOption { label:"A".into(), description:None }], multi_select:false }], wait_for_answer:true, timeout_ms:0 };
    let answer = ui.question(request, Default::default());
    mode.render(80); mode.handle_input_at("\x1b", 0);
    let response = answer.await.expect("question"); assert_eq!(response.status, maho_ext_api::QuestionStatus::Cancelled); assert_eq!(response.unanswered, vec!["item"]);
    mode.handle_input_at("draft", 1); assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[tokio::test]
async fn non_wait_question_keeps_composer_until_answer_chord() {
    use maho_ext_api::ExtensionUi; use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode(); let ui = mode.extension_ui.clone();
    let request = maho_ext_api::QuestionRequest { request_id:"async-q".into(), questions:vec![maho_ext_api::Question { id:"item".into(), header:"Header".into(), question:"Choose".into(), options:vec![maho_ext_api::QuestionOption { label:"A".into(), description:None }], multi_select:false }], wait_for_answer:false, timeout_ms:0 };
    let answer = ui.question(request, Default::default()); mode.render(80);
    mode.handle_input_at("draft", 0); assert_eq!(mode.editor.editor.get_text(), "draft");
    mode.handle_input_at("\x1ba", 1); mode.handle_input_at("\x1b", 2);
    assert_eq!(answer.await.expect("question").status, maho_ext_api::QuestionStatus::Cancelled);
    assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[tokio::test]
async fn dropped_async_question_removes_widget_and_keeps_composer() {
    use maho_ext_api::ExtensionUi; use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode(); let ui = mode.extension_ui.clone();
    let request = maho_ext_api::QuestionRequest { request_id:"dropped-q".into(), questions:vec![maho_ext_api::Question { id:"item".into(), header:"Header".into(), question:"discard-this-question".into(), options:vec![maho_ext_api::QuestionOption { label:"A".into(), description:None }], multi_select:false }], wait_for_answer:false, timeout_ms:0 };
    let answer = ui.question(request, Default::default());
    assert!(mode.render(80).join("\n").contains("discard-this-question"));
    drop(answer);
    assert!(!mode.render(80).join("\n").contains("discard-this-question"));
    mode.handle_input_at("draft", 0); assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[tokio::test]
async fn fork_selector_uses_current_user_history_and_cancel_keeps_transcript() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("fork-source", Default::default()).await.expect("turn");
    mode.submit("/fork", Default::default()).await.expect("selector");
    assert!(mode.render(80).join("\n").contains("Fork from Message"));
    mode.handle_input_at("\x1b", 0);
    mode.handle_input_at("draft", 1);
    assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[tokio::test]
async fn fork_selection_reopens_user_text_before_selected_message() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("fork-source", Default::default()).await.expect("turn");
    mode.submit("/fork", Default::default()).await.expect("selector"); mode.render(80);
    mode.handle_input_at("\r", 0); mode.submit_editor().await.expect("fork");
    assert_eq!(mode.editor.editor.get_text(), "fork-source");
    assert_eq!(mode.footer_snapshot().context_tokens, Some(0.0));
}

#[tokio::test]
async fn clone_keeps_current_leaf_transcript_and_clears_composer() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("clone-source", Default::default()).await.expect("turn");
    mode.editor.editor.set_text("draft");
    mode.submit("/clone", Default::default()).await.expect("clone");
    assert!(mode.editor.editor.get_text().is_empty());
    assert!(mode.render(80).join("\n").contains("clone-source"));
}

#[tokio::test]
async fn native_tree_selector_cancel_retains_session_and_releases_composer() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("tree-source", Default::default()).await.expect("turn");
    mode.submit("/tree", Default::default()).await.expect("tree"); mode.render(80);
    mode.handle_input_at("\x1b", 0); mode.handle_input_at("draft", 1);
    assert_eq!(mode.editor.editor.get_text(), "draft");
    assert!(mode.render(80).join("\n").contains("tree-source"));
}

#[tokio::test]
async fn fork_selector_defaults_to_latest_user_message() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("earlier-user", Default::default()).await.expect("first");
    mode.submit("latest-user", Default::default()).await.expect("second");
    mode.submit("/fork", Default::default()).await.expect("selector"); mode.render(80);
    mode.handle_input_at("\r", 0); mode.submit_editor().await.expect("fork");
    assert_eq!(mode.editor.editor.get_text(), "latest-user");
    assert!(mode.render(80).join("\n").contains("earlier-user"));
}

#[tokio::test]
async fn model_management_commands_cancel_without_consuming_provider_turn() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    for command in ["/scoped-models", "/favorite-models"] {
        assert_eq!(mode.submit(command, Default::default()).await.expect("selector"), maho_core::agent_session::PromptDisposition::Handled);
        for width in [40, 80, 120] { assert!(mode.render(width).join("\n").contains("faux-1")); }
        mode.handle_input_at("\x1b", 0);
        mode.handle_input_at("draft", 1);
        assert_eq!(mode.editor.editor.get_text(), "draft");
        mode.editor.editor.set_text("");
    }
    mode.submit("hi", Default::default()).await.expect("turn");
    assert!(mode.render(80).join("\n").contains("hello"));
}

#[tokio::test]
async fn scoped_model_selection_persists_and_restores_empty_enabled_set() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("/scoped-models", Default::default()).await.expect("selector");
    mode.render(80);
    mode.handle_input_at("\x18", 0);
    mode.handle_input_at("\x13", 1);
    mode.handle_input_at("\x1b", 2);
    mode.render(80);
    mode.submit("/scoped-models", Default::default()).await.expect("reopen");
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
    std::fs::write(directory.path().join("keybindings.json"), r#"{"app.session.renameCurrent":"ctrl+r"}"#).expect("configuration");
    mode.handle_input_at("\x12", 0);
    mode.render(80);
    mode.handle_input_at("configured-name", 1);
    mode.handle_input_at("\r", 2);
    assert_eq!(mode.footer_snapshot().session_name.as_deref(), Some("configured-name"));
}

#[tokio::test]
async fn trust_command_saves_only_future_project_decision() {
    use maho_tui::tui::Component;
    let (mut mode, directory) = native_mode();
    mode.submit("/trust", Default::default()).await.expect("trust selector");
    mode.render(80);
    mode.handle_input_at("\r", 0);
    let store = maho_core::trust_manager::ProjectTrustStore::new(&directory.path().to_string_lossy());
    assert_eq!(store.get(&directory.path().to_string_lossy()).expect("saved trust"), Some(true));
    mode.handle_input_at("draft", 1);
    assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[tokio::test]
async fn settings_menu_toggles_native_compaction_and_releases_composer() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("/settings", Default::default()).await.expect("settings");
    for width in [40, 80, 120] { mode.render(width); }
    mode.handle_input_at("\r", 0);
    mode.handle_input_at("\x1b", 1);
    mode.submit("/settings", Default::default()).await.expect("reopen");
    let rendered = mode.render(120).join("\n");
    assert!(rendered.lines().any(|line| line.contains("Auto-compact") && line.contains("false")));
    mode.handle_input_at("\x1b", 2);
    mode.handle_input_at("draft", 3);
    assert_eq!(mode.editor.editor.get_text(), "draft");
}

#[tokio::test]
async fn resume_menu_cancellation_keeps_current_native_transcript() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.submit("resume-source", Default::default()).await.expect("turn");
    assert_eq!(mode.submit("/resume", Default::default()).await.expect("resume menu"), maho_core::agent_session::PromptDisposition::Handled);
    for width in [40, 80, 120] { mode.render(width); }
    mode.handle_input_at("\x1b", 0);
    mode.handle_input_at("draft", 1);
    assert_eq!(mode.editor.editor.get_text(), "draft");
    assert!(mode.render(80).join("\n").contains("resume-source"));
}
