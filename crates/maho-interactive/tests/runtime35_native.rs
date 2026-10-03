use maho_test_support::{faux::{FauxResponse, FauxScript}, faux_session::FauxSession};

struct EditorHost;
impl maho_tui::components::editor::EditorTuiHost for EditorHost {
    fn request_render(&self) {}
    fn terminal_rows(&self) -> usize { 36 }
}

fn native_mode() -> (maho_interactive::interactive_mode::InteractiveMode, tempfile::TempDir) {
    native_mode_at(None)
}

fn native_mode_at(cwd_override:Option<&str>) -> (maho_interactive::interactive_mode::InteractiveMode, tempfile::TempDir) {
    native_mode_with_session(cwd_override, |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None))
}

fn native_mode_with_session(cwd_override:Option<&str>, session_manager: impl FnOnce(&str) -> maho_core::session_manager::SessionManager) -> (maho_interactive::interactive_mode::InteractiveMode, tempfile::TempDir) {
    use std::sync::Arc;
    use maho_ai::providers::faux::{FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_provider, faux_streams};
    use maho_core::agent_session::{AgentSession, AgentSessionConfig};
    let directory = tempfile::tempdir().expect("directory");
    let cwd = cwd_override.map_or_else(||directory.path().to_string_lossy().into_owned(),str::to_owned);
    let provider = faux_provider(RegisterFauxProviderOptions { api: Some("faux".into()), tokens_per_second: Some(0.0), ..Default::default() });
    let model = provider.get_model(Some("faux-1")).expect("model");
    provider.set_responses(vec![faux_assistant_message("hello", FauxAssistantMessageOptions { timestamp: Some(0), ..Default::default() }).into()]);
    let streams = faux_streams(provider.core.clone());
    let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| {
        let mut context=context.clone();
        context.system_prompt=context.system_prompt.filter(|prompt|!prompt.is_empty());
        streams.stream_simple(model, &context, options.map(|options| options.simple))
    });
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(directory.path().join("models.json")), auth_path: Some(directory.path().join("auth.json")),
        providers: Some(vec![provider.provider.clone()]), ..Default::default()
    });
    let session = AgentSession::new(AgentSessionConfig {
        agent: maho_agent::Agent::new(maho_agent::AgentOptions {
            initial_state: Some(maho_agent::agent::PartialAgentState { model: Some(model), ..Default::default() }),
            stream_fn: Some(stream_fn), ..Default::default()
        }),
        session_manager: session_manager(&cwd),
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
    mode.handle_input_at("continued",3);
    assert_eq!(mode.editor.editor.get_text(),"draftcontinued");
    mode.handle_input_at("\x1ba",4);
    mode.handle_input_at("\r",5);
    assert_eq!(answer.await.expect("question").status, maho_ext_api::QuestionStatus::Answered);
    assert_eq!(mode.editor.editor.get_text(), "draftcontinued");
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
async fn expanded_nonblocking_question_keeps_original_timeout() {
    use maho_ext_api::ExtensionUi; use maho_tui::tui::Component;
    let (mut mode,_directory)=native_mode();let ui=mode.extension_ui.clone();
    let request=maho_ext_api::QuestionRequest {request_id:"deadline".into(),questions:vec![maho_ext_api::Question {id:"item".into(),header:"Header".into(),question:"Choose".into(),options:vec![maho_ext_api::QuestionOption {label:"A".into(),description:None}],multi_select:false}],wait_for_answer:false,timeout_ms:1000};
    let answer=ui.question(request,Default::default());
    mode.render(80);mode.handle_input_at("\x1ba",0);mode.tick(2000.0);
    let response=answer.await.expect("deadline response");
    assert_eq!(response.status,maho_ext_api::QuestionStatus::TimedOut);
    assert_eq!(response.unanswered,["item"]);
    mode.handle_input_at("draft",2001);assert_eq!(mode.editor.editor.get_text(),"draft");
}

#[tokio::test]
async fn successive_questions_do_not_replace_an_unanswered_request() {
    use maho_ext_api::ExtensionUi; use maho_tui::tui::Component;
    let (mut mode,_directory)=native_mode(); let ui=mode.extension_ui.clone();
    let make=|id:&str|maho_ext_api::QuestionRequest { request_id:id.into(),questions:vec![maho_ext_api::Question {id:"item".into(),header:id.into(),question:"Choose".into(),options:vec![maho_ext_api::QuestionOption {label:"A".into(),description:None}],multi_select:false}],wait_for_answer:true,timeout_ms:0 };
    let first=ui.question(make("first"),Default::default());
    let second=ui.question(make("second"),Default::default());
    mode.render(80); mode.handle_input_at("1",0);
    assert_eq!(first.await.expect("first response").status,maho_ext_api::QuestionStatus::Answered);
    mode.render(80); mode.handle_input_at("1",1);
    assert_eq!(second.await.expect("second response").status,maho_ext_api::QuestionStatus::Answered);
}

#[tokio::test]
async fn answer_command_addresses_the_second_pending_question_and_settles_only_it() {
    use maho_ext_api::ExtensionUi; use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode(); let ui = mode.extension_ui.clone();
    let make = |id: &str| maho_ext_api::QuestionRequest { request_id: id.into(), questions: vec![maho_ext_api::Question { id: "item".into(), header: format!("Header {id}"), question: "Choose".into(), options: vec![maho_ext_api::QuestionOption { label: "A".into(), description: None }], multi_select: false }], wait_for_answer: false, timeout_ms: 0 };
    let first = ui.question(make("first"), Default::default());
    let second = ui.question(make("second"), Default::default());
    mode.render(80);
    assert!(mode.render(80).join("\n").contains("Header first"), "the first pending question is shown");
    mode.submit("/answer 2", Default::default()).await.expect("answer command");
    assert!(mode.render(80).join("\n").contains("Header second"), "expanding the second question");
    mode.handle_input_at("\x1b", 1);
    assert!(mode.render(80).join("\n").contains("Header second"), "Esc keeps the question pending");
    mode.handle_input_at("\x1ba", 2);
    mode.handle_input_at("1", 3);
    assert_eq!(second.await.expect("second response").status, maho_ext_api::QuestionStatus::Answered);
    assert!(mode.render(80).join("\n").contains("Header first"), "the first question is still pending");
    drop(first);
}

#[tokio::test]
async fn answer_skip_dismisses_the_shown_question_and_keeps_the_other_pending() {
    use maho_ext_api::ExtensionUi; use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode(); let ui = mode.extension_ui.clone();
    let make = |id: &str| maho_ext_api::QuestionRequest { request_id: id.into(), questions: vec![maho_ext_api::Question { id: "item".into(), header: format!("Header {id}"), question: "Choose".into(), options: vec![maho_ext_api::QuestionOption { label: "A".into(), description: None }], multi_select: false }], wait_for_answer: false, timeout_ms: 0 };
    let first = ui.question(make("first"), Default::default());
    let second = ui.question(make("second"), Default::default());
    mode.render(80);
    mode.submit("/answer 2", Default::default()).await.expect("answer command");
    mode.submit("/answer skip", Default::default()).await.expect("skip command");
    let response = second.await.expect("skipped response");
    assert_eq!(response.status, maho_ext_api::QuestionStatus::Cancelled);
    assert_eq!(response.unanswered, ["item"]);
    mode.render(80);
    assert!(mode.render(80).join("\n").contains("Header first"), "the other question stays pending");
    drop(first);
}

#[tokio::test]
async fn answer_list_numbering_and_unknown_numbers_reject_cleanly() {
    use maho_ext_api::ExtensionUi; use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode(); let ui = mode.extension_ui.clone();
    let make = |id: &str| maho_ext_api::QuestionRequest { request_id: id.into(), questions: vec![maho_ext_api::Question { id: "item".into(), header: format!("Header {id}"), question: "Choose".into(), options: vec![maho_ext_api::QuestionOption { label: "A".into(), description: None }], multi_select: false }], wait_for_answer: false, timeout_ms: 0 };
    let first = ui.question(make("first"), Default::default());
    let second = ui.question(make("second"), Default::default());
    mode.render(80);
    mode.submit("/answer 9", Default::default()).await.expect("unknown index");
    assert!(mode.render(80).join("\n").contains("Choose a pending question number or /answer skip."), "unknown numbers are rejected");
    assert!(mode.render(80).join("\n").contains("Header first"), "the shown question is unchanged");
    drop(first); drop(second);
}

fn retained_question(id: &str, wait_for_answer: bool) -> maho_ext_api::QuestionRequest {
    maho_ext_api::QuestionRequest { request_id: id.into(), questions: vec![maho_ext_api::Question { id: "item".into(), header: id.into(), question: "Choose".into(), options: vec![maho_ext_api::QuestionOption { label: "A".into(), description: None }, maho_ext_api::QuestionOption { label: "B".into(), description: None }], multi_select: true }], wait_for_answer, timeout_ms: 60_000 }
}

fn retained_options() -> maho_ext_api::QuestionOptions {
    maho_ext_api::QuestionOptions { initial_draft: Some(maho_ext_api::QuestionDraft { answers: Some([("item".into(), maho_ext_api::QuestionAnswer { selected: vec!["A".into()], text: None })].into()), comment: Some("retained-comment".into()) }), ..Default::default() }
}

#[tokio::test]
async fn blocking_question_seeds_the_producer_draft() {
    use maho_ext_api::ExtensionUi; use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode(); let ui = mode.extension_ui.clone();
    let answer = ui.question(retained_question("blocking-retained", true), retained_options());
    mode.render(80);
    mode.handle_input_at("\x1b[13;5u", 1);
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), answer).await.expect("bounded answer").expect("answer");
    assert_eq!(response.answers["item"].selected, ["A"]);
    assert_eq!(response.comment.as_deref(), Some("retained-comment"));
}

#[tokio::test]
async fn async_question_reexpansion_retains_progress_and_initial_comment() {
    use maho_ext_api::ExtensionUi; use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode(); let ui = mode.extension_ui.clone();
    let answer = ui.question(retained_question("async-retained", false), retained_options());
    mode.render(80); mode.submit("/answer", Default::default()).await.expect("expand");
    mode.handle_input_at("2", 1); mode.handle_input_at("\x1b", 2);
    mode.submit("/answer", Default::default()).await.expect("reexpand");
    mode.handle_input_at("\x1b[13;5u", 3);
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), answer).await.expect("bounded answer").expect("answer");
    assert_eq!(response.answers["item"].selected, ["A", "B"]);
    assert_eq!(response.comment.as_deref(), Some("retained-comment"));
}

#[tokio::test]
async fn answer_list_selection_expands_the_selected_request() {
    use maho_ext_api::ExtensionUi; use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode(); let ui = mode.extension_ui.clone();
    let first = ui.question(retained_question("first-list", false), retained_options());
    let second = ui.question(retained_question("second-list", false), retained_options());
    mode.render(80); mode.submit("/answer", Default::default()).await.expect("list");
    mode.handle_input_at("\x1b[B", 1); mode.handle_input_at("\r", 2); mode.render(80);
    mode.handle_input_at("\x1b[13;5u", 3);
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), second).await.expect("bounded selection").expect("selected answer");
    assert_eq!(response.status, maho_ext_api::QuestionStatus::Answered);
    assert_eq!(response.answers["item"].selected, ["A"]);
    drop(first);
}

#[tokio::test(start_paused = true)]
async fn live_deadline_does_not_use_the_fixed_attachment_timeout() {
    use maho_ext_api::ExtensionUi;
    let theme = maho_interactive::theme::Theme::builtin("dark", maho_interactive::theme::ColorMode::Truecolor).expect("theme");
    let (ui, mut requests) = maho_interactive::interactive_extension_ui::InteractiveExtensionUi::channel(theme);
    let signal = maho_ext_api::AbortSignal::default();
    let answer = ui.question(retained_question("live", false), maho_ext_api::QuestionOptions { dialog: maho_ext_api::ExtensionUiDialogOptions { signal: Some(signal.clone()), timeout_ms: Some(1) }, get_deadline_at_ms: Some(std::sync::Arc::new(|| 100_000)), ..Default::default() });
    let mut answer = Box::pin(answer);
    let maho_interactive::interactive_extension_ui::UiRequest::Question { reply, .. } = requests.recv().await.expect("question request") else { panic!("question request"); };
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(answer.as_mut().poll(cx))).await.is_pending());
    tokio::time::advance(std::time::Duration::from_millis(2)).await;
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(answer.as_mut().poll(cx))).await.is_pending());
    signal.abort();
    assert_eq!(answer.await.expect("abort").status, maho_ext_api::QuestionStatus::Cancelled);
    drop(reply);
}

#[tokio::test]
async fn collapsed_and_expanded_countdowns_read_the_live_capped_deadline() {
    use maho_ext_api::ExtensionUi; use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode(); let ui = mode.extension_ui.clone();
    let live = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX)); let source = live.clone();
    let signal = maho_ext_api::AbortSignal::default();
    let answer = ui.question(retained_question("display-live", false), maho_ext_api::QuestionOptions { dialog: maho_ext_api::ExtensionUiDialogOptions { signal: Some(signal.clone()), timeout_ms: Some(1) }, hard_deadline_at_ms: Some(1), get_deadline_at_ms: Some(std::sync::Arc::new(move || source.load(std::sync::atomic::Ordering::Relaxed))), ..retained_options() });
    assert!(mode.render(80).join("\n").contains("00:00"));
    mode.submit("/answer", Default::default()).await.expect("expand");
    assert!(mode.render(80).join("\n").contains("00:00"));
    mode.handle_input_at("\x1b", 1); mode.render(80);
    signal.abort(); assert_eq!(answer.await.expect("producer abort").status, maho_ext_api::QuestionStatus::Cancelled);
}

#[tokio::test]
async fn reattached_question_replaces_stale_draft_without_changing_number() {
    use maho_ext_api::ExtensionUi; use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode(); let ui = mode.extension_ui.clone();
    let old = ui.question(retained_question("reattach", false), Default::default());
    let other = ui.question(retained_question("other", false), Default::default());
    mode.render(80); drop(old);
    let replacement = ui.question(retained_question("reattach", false), retained_options());
    mode.render(80); mode.submit("/answer 1", Default::default()).await.expect("first position");
    mode.handle_input_at("\x1b[13;5u", 1);
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), replacement).await.expect("bounded replacement").expect("replacement");
    assert_eq!(response.answers["item"].selected, ["A"]);
    assert_eq!(response.comment.as_deref(), Some("retained-comment"));
    drop(other);
}

#[tokio::test]
async fn ask_user_tool_card_uses_the_question_renderer_not_a_raw_argument_dump() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.handle_event(&maho_agent::types::AgentEvent::ToolExecutionStart {
        tool_call_id: "call-ask".into(), tool_name: "ask_user_question".into(),
        args: serde_json::json!({"questions": [{"header": "Pick one"}], "waitForAnswer": true}),
    });
    let text = mode.render(80).join("\n");
    assert!(text.contains("[Pick one]"), "question headers: {text}");
    assert!(text.contains("wait for answer"), "wait mode: {text}");
    assert!(!text.contains("\"questions\""), "the raw argument dump is replaced: {text}");
}

#[tokio::test]
async fn ask_user_tool_result_summarises_the_answer_details() {
    use maho_tui::tui::Component;
    let (mut mode, _directory) = native_mode();
    mode.handle_event(&maho_agent::types::AgentEvent::ToolExecutionStart { tool_call_id: "call-ask".into(), tool_name: "ask_user_question".into(), args: serde_json::json!({"questions": [{"header": "Pick one"}]}) });
    mode.handle_event(&maho_agent::types::AgentEvent::ToolExecutionEnd {
        tool_call_id: "call-ask".into(), tool_name: "ask_user_question".into(),
        result: serde_json::json!({"content": [{"type": "text", "text": "The user responded"}], "details": {"status": "answered", "answers": {"a": "x"}, "unanswered": ["b"]}}),
        is_error: false,
    });
    let text = mode.render(80).join("\n");
    assert!(text.contains("answered; 1 answered; 1 unanswered"), "result summary: {text}");
    assert!(text.contains("The user responded"), "result content: {text}");
}

#[tokio::test]
async fn native_tree_copy_keeps_selector_open_and_does_not_prompt() {
    use maho_tui::tui::Component;
    let (mut mode,_directory)=native_mode();
    mode.submit("hi",Default::default()).await.expect("seed turn");
    mode.submit("/tree",Default::default()).await.expect("tree");
    let before=mode.render(80);
    mode.handle_runtime_input("\x18",0).await.expect("copy key");
    assert!(mode.submit_editor().await.expect("copy action").is_none());
    mode.handle_runtime_input("\x1b",1).await.expect("close tree");
    mode.handle_input_at("draft",2);
    assert_eq!(mode.editor.editor.get_text(),"draft");
    assert!(!before.is_empty());
    assert!(mode.agent_idle);
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

#[tokio::test]
async fn image_payloads_follow_submitted_marker_order() {
    use maho_tui::tui::Component;
    let (mut mode, directory) = native_mode();
    mode.attach_image(maho_ai::types::ImageContent { data:"first".into(), mime_type:"image/png".into() });
    mode.handle_input_at("\x1b[H", 0);
    mode.attach_image(maho_ai::types::ImageContent { data:"second".into(), mime_type:"image/png".into() });
    mode.handle_input("\r");
    mode.submit_editor().await.expect("image prompt");
    let path = directory.path().join("images.jsonl");
    mode.submit(&format!("/export {}", path.display()), Default::default()).await.expect("export");
    let entries: Vec<serde_json::Value> = std::fs::read_to_string(path).expect("exported session").lines()
        .map(|line| serde_json::from_str(line).expect("entry")).collect();
    let user = entries.iter().find(|entry| entry["message"]["role"] == "user").expect("user message");
    let images: Vec<_> = user["message"]["content"].as_array().expect("content").iter()
        .filter(|part| part["type"] == "image").map(|part| part["data"].as_str().expect("image data")).collect();
    assert_eq!(images, ["second", "first"]);
}

struct ScreenTerminal {
    writes:String,
    screen: maho_test_support::vterm::VirtualTerminal,
    input: Option<maho_tui::terminal::InputHandler>,
    stopped: bool,
}

#[tokio::test]
async fn faux_screen_hi_matches_pinned_interactive_cells() {
    for (width,fixture) in [(40,"senpi-hi-40.cells.json"),(80,"senpi-hi-80.cells.json"),(120,"senpi-hi.cells.json")] {
    let (mut mode,_directory)=native_mode_at(Some("/tmp"));
    mode.handle_input_at("hi",0);mode.handle_input_at("\r",1);
    mode.submit_editor().await.expect("native hi");
    let theme=maho_interactive::theme::Theme::builtin("dark",maho_interactive::theme::ColorMode::Truecolor).expect("theme");
    let renderer=maho_interactive::tui_renderer::create_interactive_tui(maho_interactive::tui_renderer::InteractiveTuiOptions {tui_mode:maho_interactive::tui_renderer::TuiMode::Fullscreen,show_hardware_cursor:false,bottom_shortcut:String::new()},theme);
    let mut mounted=maho_interactive::interactive_terminal::InteractiveTerminal::new(mode,renderer);
    let mut terminal=ScreenTerminal {writes:String::new(),screen:maho_test_support::vterm::VirtualTerminal::new(width,36),input:None,stopped:false};
    mounted.start(&mut terminal,false,false);
    let fixture=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.omo/evidence/task-35-faux").join(fixture);
    let expected:maho_test_support::vterm::Screen=serde_json::from_str(&std::fs::read_to_string(fixture).expect("pinned fixture")).expect("pinned cells");
    let actual=terminal.screen.snapshot();
    println!("NATIVE_HI_ANSI_{width}={}",serde_json::to_string(&terminal.writes).expect("ANSI evidence"));
    mounted.stop(&mut terminal,true).expect("restore terminal");
    assert_eq!(actual.cells,expected.cells,"native viewport: {:?}",actual.viewport);
    }
}
#[tokio::test]
async fn faux_screen_slash_hotkeys_matches_pinned_cells() {
    let (mut mode,_directory)=native_mode_at(Some("/tmp"));
    mode.handle_input_at("hi",0);mode.handle_input_at("\r",1);
    mode.submit_editor().await.expect("seed turn");
    mode.handle_input_at("/hotkeys",2);mode.handle_input_at("\r",3);
    mode.submit_editor().await.expect("hotkeys");
    let theme=maho_interactive::theme::Theme::builtin("dark",maho_interactive::theme::ColorMode::Truecolor).expect("theme");
    let renderer=maho_interactive::tui_renderer::create_interactive_tui(maho_interactive::tui_renderer::InteractiveTuiOptions {tui_mode:maho_interactive::tui_renderer::TuiMode::Fullscreen,show_hardware_cursor:false,bottom_shortcut:String::new()},theme);
    let mut mounted=maho_interactive::interactive_terminal::InteractiveTerminal::new(mode,renderer);
    let mut terminal=ScreenTerminal {writes:String::new(),screen:maho_test_support::vterm::VirtualTerminal::new(120,36),input:None,stopped:false};
    mounted.start(&mut terminal,false,false);
    let expected:maho_test_support::vterm::Screen=serde_json::from_str(include_str!("../../../.omo/evidence/task-35-slash/senpi-hotkeys-120.cells.json")).expect("pinned cells");
    let actual=terminal.screen.snapshot();
    println!("NATIVE_HOTKEYS_ANSI={}",serde_json::to_string(&terminal.writes).expect("ANSI evidence"));
    mounted.stop(&mut terminal,true).expect("restore terminal");
    assert_eq!(actual.cells,expected.cells,"native viewport: {:?}",actual.viewport);
}

impl maho_tui::terminal::Terminal for ScreenTerminal {
    fn start(&mut self, input:maho_tui::terminal::InputHandler, _:maho_tui::terminal::ResizeHandler) { self.input=Some(input); self.screen.start(); }
    fn stop(&mut self) -> Result<(), maho_tui::terminal::TerminalError> { self.input=None; self.stopped=true; self.screen.stop(); Ok(()) }
    fn drain_input(&mut self, _:u64, _:u64) {}
    fn write(&mut self, data:&str) { self.writes.push_str(data);self.screen.write(data); }
    fn columns(&self)->u16 { self.screen.columns() }
    fn rows(&self)->u16 { self.screen.rows() }
    fn kitty_protocol_active(&self)->bool { true }
    fn move_by(&mut self, lines:i64) { self.screen.move_by(i32::try_from(lines).expect("lines")); }
    fn hide_cursor(&mut self) { self.screen.hide_cursor(); }
    fn show_cursor(&mut self) { self.screen.show_cursor(); }
    fn clear_line(&mut self) { self.screen.clear_line(); }
    fn clear_from_cursor(&mut self) { self.screen.clear_from_cursor(); }
    fn clear_screen(&mut self) { self.screen.clear_screen(); }
    fn set_title(&mut self, title:&str) { self.screen.set_title(title); }
    fn set_progress(&mut self, _:bool) {}
}

#[tokio::test]
async fn mounted_terminal_renders_native_turn_and_restores_terminal_on_stop() {
    for width in [40,80,120] {
        let (mut mode, _directory) = native_mode();
        mode.submit("hi",Default::default()).await.expect("turn");
        let theme = maho_interactive::theme::Theme::builtin("dark", maho_interactive::theme::ColorMode::Truecolor).expect("theme");
        let renderer = maho_interactive::tui_renderer::create_interactive_tui(maho_interactive::tui_renderer::InteractiveTuiOptions {
            tui_mode:maho_interactive::tui_renderer::TuiMode::Fullscreen, show_hardware_cursor:false, bottom_shortcut:String::new(),
        }, theme);
        let mut mounted = maho_interactive::interactive_terminal::InteractiveTerminal::new(mode, renderer);
        let mut terminal = ScreenTerminal { writes:String::new(),screen:maho_test_support::vterm::VirtualTerminal::new(width,36), input:None, stopped:false };
        mounted.start(&mut terminal,false,false);
        let dimensions=std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured=dimensions.clone();
        {
            use maho_ext_api::ExtensionUi;
            mounted.mode.borrow().extension_ui.set_header_factory(Some(std::sync::Arc::new(move |host,_| {
                *captured.lock().expect("dimensions")=Some(host.dimensions());
                Box::new(maho_tui::components::text::Text::new("header"))
            }))).expect("header factory");
        }
        mounted.render(&mut terminal);
        assert_eq!(*dimensions.lock().expect("dimensions"),Some((width,36)));
        terminal.input.as_mut().expect("input")("draft");
        while let Some(input) = mounted.take_input(&mut terminal,0) { mounted.mode.borrow_mut().handle_input_at(&input,0); }
        assert_eq!(mounted.mode.borrow().editor.editor.get_text(),"draft");
        mounted.render(&mut terminal);
        assert!(terminal.screen.viewport().join("\n").contains("hello"));
        assert!(terminal.screen.bracketed_paste());
        mounted.stop(&mut terminal,true).expect("stop");
        assert!(terminal.stopped);
        assert!(!terminal.screen.bracketed_paste());
        assert!(!terminal.screen.cursor_hidden());
    }
}

#[tokio::test]
async fn extension_actions_expose_theme_catalog_and_live_working_indicator() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode,_directory) = native_mode();
    let ui = mode.extension_ui.clone();
    assert!(ui.get_all_themes().expect("themes").iter().any(|theme|theme.name=="light"));
    assert!(ui.set_theme(maho_ext_api::ThemeSelection::Name("light".into())).expect("theme result").success);
    mode.render(80);
    assert_eq!(ui.theme().name.as_deref(),Some("light"));
    ui.set_working_indicator(Some(maho_ext_api::WorkingIndicatorOptions { frames:Some(vec!["frame-a".into(),"frame-b".into()]), interval_ms:Some(10) })).expect("indicator");
    mode.render(80);
    mode.handle_event(&maho_agent::types::AgentEvent::AgentStart);
    assert!(mode.working_frame(0.0).is_some_and(|frame|frame=="frame-a"));
}

struct CustomChoice {
    done: maho_ext_api::CustomUiDone,
}
impl maho_tui::tui::Component for CustomChoice {
    fn render(&mut self,_:usize)->Vec<String> { vec!["custom-choice".into()] }
    fn handle_input(&mut self,_:&str) { (self.done)(serde_json::json!({"selected":7})); }
    fn has_input_handler(&self)->bool { true }
}

#[tokio::test]
async fn custom_factory_completion_restores_editor_and_returns_value() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode,_directory)=native_mode();
    let ui=mode.extension_ui.clone();
    let factory:maho_ext_api::CustomComponentFactory=std::sync::Arc::new(|_,_,_,done|Box::pin(async move { Ok(Box::new(CustomChoice{done}) as Box<dyn Component>) }));
    let result=ui.custom_factory(factory,Default::default());
    assert!(mode.render(80).join("\n").contains("custom-choice"));
    mode.handle_input_at("select",0);
    mode.render(80);
    assert_eq!(result.await.expect("completed factory"),serde_json::json!({"selected":7}));
    mode.handle_input_at("draft",1);
    assert_eq!(mode.editor.editor.get_text(),"draft");
}

#[tokio::test]
async fn debug_command_writes_the_rendered_report_to_the_configured_path() {
    use maho_tui::tui::Component;
    let (mut mode,directory) = native_mode();
    let path = directory.path().join("debug.log");
    mode.set_debug_log_path(Some(path.to_string_lossy().into_owned()));
    mode.submit("/debug",Default::default()).await.expect("debug command");
    let data = std::fs::read_to_string(&path).expect("debug log");
    assert!(data.contains("Terminal: "),"terminal size line");
    assert!(data.contains("=== All rendered lines with visible widths ==="),"render dump header");
    assert!(data.contains("=== Agent messages (JSONL) ==="),"message dump header");
    assert!(mode.render(80).join("\n").contains("Debug log written"),"status feedback");
}

#[tokio::test]
async fn import_command_confirms_copies_and_switches_to_the_jsonl() {
    use maho_tui::tui::Component;
    let (mut mode,directory) = native_mode_with_session(None, |cwd| {
        let session_dir = std::path::Path::new(cwd).join("sessions");
        std::fs::create_dir_all(&session_dir).expect("session dir");
        let session_dir = session_dir.to_string_lossy().into_owned();
        maho_core::session_manager::SessionManager::create(cwd, Some(&session_dir), None)
    });
    let source_dir = tempfile::tempdir().expect("source directory");
    let source = source_dir.path().join("imported.jsonl");
    let cwd = directory.path().to_string_lossy().into_owned();
    let header = serde_json::json!({"type":"session","version":3,"id":"imported","timestamp":"2026-10-03T00:00:00.000Z","cwd":cwd});
    let message = serde_json::json!({"type":"message","id":"m1","parentId":null,"timestamp":"2026-10-03T00:00:01.000Z","message":{"role":"user","content":"imported prompt"}});
    std::fs::write(&source,format!("{header}\n{message}\n")).expect("fixture");
    mode.submit(&format!("/import {}",source.display()),Default::default()).await.expect("confirm dialog");
    assert!(mode.render(80).join("\n").contains("Replace current session with"),"confirmation prompt");
    mode.handle_input_at("\r",0);
    mode.submit_editor().await.expect("import command");
    let text = mode.render(80).join("\n");
    assert!(text.contains("imported prompt"),"imported transcript: {text}");
    assert!(text.contains("Session imported from:"),"import status");
}

#[tokio::test]
async fn import_command_reports_a_missing_file_before_any_session_change() {
    let (mut mode,directory) = native_mode();
    let missing = directory.path().join("absent.jsonl");
    assert_eq!(mode.submit(&format!("/import {}",missing.display()),Default::default()).await.expect("confirm dialog"),maho_core::agent_session::PromptDisposition::Handled);
    mode.handle_input_at("\r",0);
    let error = mode.submit_editor().await.expect_err("missing import file");
    assert_eq!(error,format!("File not found: {}",missing.display()));
}

#[tokio::test]
async fn import_without_arguments_reports_usage() {
    let (mut mode,_directory) = native_mode();
    assert_eq!(mode.submit("/import",Default::default()).await.expect_err("usage"),"Usage: /import <path.jsonl>");
}

#[tokio::test]
async fn startup_changelog_notices_show_only_new_entries_and_record_the_seen_version() {
    use maho_tui::tui::Component;
    let (mut mode,directory) = native_mode();
    let path = directory.path().join("CHANGELOG.md");
    std::fs::write(&path,"## 0.82.0 - 2026-01-03\nNewest entry\n\n## 0.81.0 - 2026-01-02\nOlder entry\n").expect("changelog");
    let source = |version: &str| maho_core::changelog_source::ChangelogSource { id:"engine".into(), path:path.to_string_lossy().into_owned(), version:Some(version.into()), rewrite_links:false };
    // A fresh install records the version without showing anything.
    mode.load_startup_changelog_from(&source("0.81.0"));
    assert!(mode.changelog_markdown().is_none(),"a fresh install shows no changelog");
    assert_eq!(mode.changelog_seen("engine").as_deref(),Some("0.81.0"),"the first-seen version is recorded");
    // A later build shows only the entries newer than the recorded version.
    mode.load_startup_changelog_from(&source("0.82.0"));
    assert!(mode.changelog_markdown().is_some_and(|markdown|markdown.contains("Newest entry")),"the newer entry is pending");
    mode.show_startup_notices_if_needed();
    let text = mode.render(80).join("\n");
    assert!(text.contains("What's New"),"heading: {text}");
    assert!(text.contains("Newest entry"),"new entry body: {text}");
    assert!(!text.contains("Older entry"),"the already-seen entry is hidden: {text}");
    assert_eq!(mode.changelog_seen("engine").as_deref(),Some("0.82.0"),"the newer version is recorded");
    // The notices render once.
    mode.show_startup_notices_if_needed();
    assert_eq!(mode.render(80).join("\n").matches("What's New").count(),1,"startup notices render only once");
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
    mode.submit("/tree-edit u1", Default::default()).await.expect("command");
    assert!(mode.render(80).join("\n").contains("Only assistant responses can be edited here"), "non-assistant entries are rejected");
    // An assistant entry opens the editor prefilled with its text.
    mode.submit("/tree-edit a1", Default::default()).await.expect("command");
    assert!(mode.render(80).join("\n").contains("assistant reply"), "the editor is prefilled with the assistant text");
}

#[tokio::test]
async fn tree_edit_apply_without_a_pending_edit_is_a_noop() {
    let (mut mode,_directory) = native_mode();
    mode.submit("/tree-edit-apply", Default::default()).await.expect("no-op");
}

#[tokio::test]
async fn tree_navigate_without_a_pending_choice_is_a_noop() {
    let (mut mode,_directory) = native_mode();
    mode.submit("/tree-navigate", Default::default()).await.expect("no-op");
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
    mode.submit("/tree u1", Default::default()).await.expect("command");
    let text = mode.render(80).join("\n");
    assert!(text.contains("Summarize branch?"), "the branch summary selector mounts: {text}");
    assert!(text.contains("Summarize with custom prompt"), "the custom option is offered: {text}");
}

#[tokio::test]
async fn startup_changelog_records_without_showing_on_a_fresh_install() {
    use maho_tui::tui::Component;
    let (mut mode,directory) = native_mode();
    let path = directory.path().join("CHANGELOG.md");
    std::fs::write(&path,"## 0.82.0 - 2026-01-03\nNewest entry\n").expect("changelog");
    let source = maho_core::changelog_source::ChangelogSource { id:"engine".into(), path:path.to_string_lossy().into_owned(), version:Some("0.82.0".into()), rewrite_links:false };
    mode.load_startup_changelog_from(&source);
    mode.show_startup_notices_if_needed();
    // With no recorded version the install is fresh: record it, show nothing.
    assert_eq!(mode.changelog_seen("engine").as_deref(),Some("0.82.0"));
    assert!(!mode.render(80).join("\n").contains("What's New"),"a fresh install shows no notices");
}

#[tokio::test]
async fn startup_header_mounts_the_welcome_content_with_a_tip_and_follows_the_toggle() {
    use maho_tui::tui::Component;
    let (mut mode,_directory) = native_mode();
    mode.mount_startup_header_at(0);
    let collapsed = mode.render(80).join("\n");
    let app = maho_core::config::app_name();
    assert!(!app.is_empty());
    assert!(collapsed.contains(app.as_str()), "the welcome logo names the app: {collapsed}");
    assert!(collapsed.contains("can explain its own features"), "onboarding: {collapsed}");
    assert!(collapsed.contains("to show full startup help and loaded resources"), "collapsed help: {collapsed}");
    assert!(collapsed.contains("Tip:"), "the tip sibling renders: {collapsed}");
    assert!(!mode.tips_history().is_empty(), "the shown tip is recorded in tipsHistory");
    assert!(!collapsed.contains("to interrupt"), "the expanded instructions stay hidden: {collapsed}");
    mode.set_tools_expanded(true);
    let expanded = mode.render(80).join("\n");
    assert!(expanded.contains("to interrupt"), "the expansion toggle swaps in the full instructions: {expanded}");
    assert!(!expanded.contains("to show full startup help and loaded resources"), "the collapsed hint is replaced: {expanded}");
}

#[tokio::test]
async fn extension_header_replaces_the_built_in_header_and_restores_it() {
    use maho_ext_api::ExtensionUi; use maho_tui::tui::Component;
    let (mut mode,_directory) = native_mode();
    mode.mount_startup_header_at(0);
    assert!(mode.render(80).join("\n").contains("can explain its own features"), "the built-in header mounts first");
    let ui = mode.extension_ui.clone();
    ui.set_header_factory(Some(std::sync::Arc::new(|_,_| Box::new(maho_tui::components::text::Text::new("CUSTOM HEADER"))))).expect("header factory");
    let with_custom = mode.render(80).join("\n");
    assert!(with_custom.contains("CUSTOM HEADER"), "the extension header renders: {with_custom}");
    assert!(!with_custom.contains("can explain its own features"), "the built-in header is replaced: {with_custom}");
    ui.set_header_factory(None).expect("header factory cleared");
    assert!(mode.render(80).join("\n").contains("can explain its own features"), "the built-in header is restored");
}

#[tokio::test]
async fn replacement_commands_route_through_the_mounted_session_host() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use maho_interactive::interactive_session::{ForkOutcome, InteractiveSession, ReplacementOutcome, SessionFuture};

    struct SpyHost { calls: Rc<RefCell<Vec<String>>> }
    impl InteractiveSession for SpyHost {
        fn session_id(&self) -> Option<String> { None }
        fn session_file(&self) -> Option<String> { None }
        fn cwd(&self) -> String { String::new() }
        fn is_reconnecting(&self) -> bool { false }
        fn is_fallback(&self) -> bool { false }
        fn remote_state(&self) -> Option<maho_interactive::interactive_host_runtime::RemoteSessionState> { None }
        fn subscribe_session_events(&self, _listener: maho_interactive::interactive_host_runtime::SessionEventListener) -> Option<maho_interactive::interactive_host_runtime::SessionSubscription> { None }
        fn request_remote_history(&self, _generation: u64, _sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>) {}
        fn new_session(&self, _parent_session: Option<String>) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            let calls = self.calls.clone();
            Box::pin(async move { calls.borrow_mut().push("new_session".into()); Ok(ReplacementOutcome::Replaced) })
        }
        fn switch_session(&self, path: String) -> SessionFuture<'_, Result<ReplacementOutcome, String>> {
            let calls = self.calls.clone();
            Box::pin(async move { calls.borrow_mut().push(format!("switch_session:{path}")); Ok(ReplacementOutcome::Replaced) })
        }
        fn fork(&self, entry_id: String, include_entry: bool) -> SessionFuture<'_, Result<ForkOutcome, String>> {
            let calls = self.calls.clone();
            Box::pin(async move { calls.borrow_mut().push(format!("fork:{entry_id}:{include_entry}")); Ok(ForkOutcome { outcome: ReplacementOutcome::Replaced, editor_text: Some("selected".into()) }) })
        }
        fn dispose(&self) -> SessionFuture<'_, ()> { Box::pin(async {}) }
    }

    let (mut mode, _directory) = native_mode();
    let calls = Rc::new(RefCell::new(Vec::new()));
    mode.set_session_host(Arc::new(SpyHost { calls: calls.clone() }));
    mode.submit("/new", Default::default()).await.expect("new");
    assert_eq!(calls.borrow().as_slice(), ["new_session"], "the mounted host receives /new instead of the local session");
    mode.submit("/fork entry-1", Default::default()).await.expect("fork");
    assert_eq!(calls.borrow().as_slice(), ["new_session", "fork:entry-1:false"], "the mounted host receives /fork");
    assert_eq!(mode.editor.editor.get_text(), "selected", "the host fork's selected text prefills the editor");
}

#[tokio::test]
async fn mounted_host_session_events_flow_into_the_mode() {
    use std::sync::{Arc, Mutex};
    use maho_interactive::interactive_host_runtime::SessionEventListener;
    use maho_interactive::interactive_session::{ForkOutcome, InteractiveSession, ReplacementOutcome, SessionFuture};

    struct BridgeHost { listener: Arc<Mutex<Option<SessionEventListener>>> }
    impl InteractiveSession for BridgeHost {
        fn session_id(&self) -> Option<String> { None }
        fn session_file(&self) -> Option<String> { None }
        fn cwd(&self) -> String { String::new() }
        fn is_reconnecting(&self) -> bool { false }
        fn is_fallback(&self) -> bool { false }
        fn remote_state(&self) -> Option<maho_interactive::interactive_host_runtime::RemoteSessionState> { None }
        fn subscribe_session_events(&self, listener: SessionEventListener) -> Option<maho_interactive::interactive_host_runtime::SessionSubscription> {
            *self.listener.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(listener);
            None
        }
        fn request_remote_history(&self, _generation: u64, _sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>) {}
        fn new_session(&self, _parent_session: Option<String>) -> SessionFuture<'_, Result<ReplacementOutcome, String>> { Box::pin(async { Ok(ReplacementOutcome::Cancelled) }) }
        fn switch_session(&self, _session_path: String) -> SessionFuture<'_, Result<ReplacementOutcome, String>> { Box::pin(async { Ok(ReplacementOutcome::Cancelled) }) }
        fn fork(&self, _entry_id: String, _include_entry: bool) -> SessionFuture<'_, Result<ForkOutcome, String>> { Box::pin(async { Ok(ForkOutcome { outcome: ReplacementOutcome::Cancelled, editor_text: None }) }) }
        fn dispose(&self) -> SessionFuture<'_, ()> { Box::pin(async {}) }
    }

    let (mut mode, _directory) = native_mode();
    let listener = Arc::new(Mutex::new(None));
    mode.set_session_host(Arc::new(BridgeHost { listener: listener.clone() }));
    let emit = listener.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone().expect("the mode subscribed its event bridge");
    emit(&maho_ext_api::AgentSessionEvent::ContinuationError { error_message: "remote-bridge-error".into() });
    assert!(mode.render(80).join("\n").contains("remote-bridge-error"), "the mounted host's session events drive the mode");
}

#[tokio::test]
async fn mounted_host_publishes_authoritative_remote_history_without_local_fallback() {
    use std::sync::{Arc, Mutex};
    use maho_interactive::interactive_host_runtime::{SessionEventListener, SessionSubscription};
    use maho_interactive::interactive_session::{ForkOutcome, InteractiveSession, ReplacementOutcome, SessionFuture};

    struct HistoryHost {
        listener: Arc<Mutex<Option<SessionEventListener>>>,
        generation: Arc<Mutex<Option<u64>>>,
        sender: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>>>>,
    }
    impl InteractiveSession for HistoryHost {
        fn session_id(&self) -> Option<String> { None }
        fn session_file(&self) -> Option<String> { None }
        fn cwd(&self) -> String { String::new() }
        fn is_reconnecting(&self) -> bool { false }
        fn is_fallback(&self) -> bool { false }
        fn remote_state(&self) -> Option<maho_interactive::interactive_host_runtime::RemoteSessionState> { None }
        fn subscribe_session_events(&self, listener: SessionEventListener) -> Option<SessionSubscription> { *self.listener.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(listener); None }
        fn request_remote_history(&self, generation: u64, sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>) {
            *self.generation.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(generation);
            *self.sender.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
        }
        fn new_session(&self, _parent_session: Option<String>) -> SessionFuture<'_, Result<ReplacementOutcome, String>> { Box::pin(async { Ok(ReplacementOutcome::Replaced) }) }
        fn switch_session(&self, _session_path: String) -> SessionFuture<'_, Result<ReplacementOutcome, String>> { Box::pin(async { Ok(ReplacementOutcome::Replaced) }) }
        fn fork(&self, _entry_id: String, _include_entry: bool) -> SessionFuture<'_, Result<ForkOutcome, String>> { Box::pin(async { Ok(ForkOutcome { outcome: ReplacementOutcome::Replaced, editor_text: None }) }) }
        fn dispose(&self) -> SessionFuture<'_, ()> { Box::pin(async {}) }
    }

    fn host() -> (Arc<HistoryHost>, Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>>>>, Arc<Mutex<Option<u64>>>, Arc<Mutex<Option<SessionEventListener>>>) {
        let host = Arc::new(HistoryHost { listener: Arc::new(Mutex::new(None)), generation: Arc::new(Mutex::new(None)), sender: Arc::new(Mutex::new(None)) });
        (host.clone(), host.sender.clone(), host.generation.clone(), host.listener.clone())
    }

    let (mut mode, _directory) = native_mode_with_session(None, |cwd| {
        let mut manager = maho_core::session_manager::SessionManager::in_memory(cwd, None, None);
        manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"local-only"}],"timestamp":0}));
        manager
    });
    let (host, sender, generation, _listener) = host();
    mode.set_session_host(host);
    mode.rebuild_history();
    assert!(!mode.render(80).join("\n").contains("local-only"), "a mounted host never falls back to the local session");
    let generation = generation.lock().unwrap_or_else(std::sync::PoisonError::into_inner).expect("the mount requested history");
    sender.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().expect("the mount kept the sender").send((generation, Ok(vec![serde_json::json!({"role":"user","content":[{"type":"text","text":"remote-history"}],"timestamp":0})]))).expect("send history");
    mode.drain_events();
    let rendered = mode.render(80).join("\n");
    assert!(rendered.contains("remote-history"), "the published snapshot rebuilds the transcript: {rendered}");
    assert!(!rendered.contains("local-only"), "the local session is not mixed in");
}

#[tokio::test]
async fn stale_history_fetch_completion_is_discarded() {
    use std::sync::{Arc, Mutex};
    use maho_interactive::interactive_host_runtime::{SessionEventListener, SessionSubscription};
    use maho_interactive::interactive_session::{ForkOutcome, InteractiveSession, ReplacementOutcome, SessionFuture};

    struct StaleHost { generation: Arc<Mutex<Option<u64>>>, sender: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>>>> }
    impl InteractiveSession for StaleHost {
        fn session_id(&self) -> Option<String> { None }
        fn session_file(&self) -> Option<String> { None }
        fn cwd(&self) -> String { String::new() }
        fn is_reconnecting(&self) -> bool { false }
        fn is_fallback(&self) -> bool { false }
        fn remote_state(&self) -> Option<maho_interactive::interactive_host_runtime::RemoteSessionState> { None }
        fn subscribe_session_events(&self, _listener: SessionEventListener) -> Option<SessionSubscription> { None }
        fn request_remote_history(&self, generation: u64, sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>) {
            *self.generation.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(generation);
            *self.sender.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
        }
        fn new_session(&self, _parent_session: Option<String>) -> SessionFuture<'_, Result<ReplacementOutcome, String>> { Box::pin(async { Ok(ReplacementOutcome::Replaced) }) }
        fn switch_session(&self, _session_path: String) -> SessionFuture<'_, Result<ReplacementOutcome, String>> { Box::pin(async { Ok(ReplacementOutcome::Replaced) }) }
        fn fork(&self, _entry_id: String, _include_entry: bool) -> SessionFuture<'_, Result<ForkOutcome, String>> { Box::pin(async { Ok(ForkOutcome { outcome: ReplacementOutcome::Replaced, editor_text: None }) }) }
        fn dispose(&self) -> SessionFuture<'_, ()> { Box::pin(async {}) }
    }

    let (mut mode, _directory) = native_mode();
    let generation = Arc::new(Mutex::new(None));
    let sender = Arc::new(Mutex::new(None));
    mode.set_session_host(Arc::new(StaleHost { generation: generation.clone(), sender: sender.clone() }));
    let first = generation.lock().unwrap_or_else(std::sync::PoisonError::into_inner).expect("first request");
    mode.submit("/new", Default::default()).await.expect("replacement");
    let second = generation.lock().unwrap_or_else(std::sync::PoisonError::into_inner).expect("replacement request");
    assert!(second > first, "a replacement bumps the generation");
    sender.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().expect("sender").send((first, Ok(vec![serde_json::json!({"role":"user","content":[{"type":"text","text":"stale-history"}],"timestamp":0})]))).expect("send stale");
    mode.drain_events();
    assert!(!mode.render(80).join("\n").contains("stale-history"), "a completion for a superseded generation is discarded");
}

#[tokio::test]
async fn remote_live_event_during_history_fetch_is_still_applied() {
    use std::sync::{Arc, Mutex};
    use maho_interactive::interactive_host_runtime::{SessionEventListener, SessionSubscription};
    use maho_interactive::interactive_session::{ForkOutcome, InteractiveSession, ReplacementOutcome, SessionFuture};

    struct LiveHost { listener: Arc<Mutex<Option<SessionEventListener>>>, generation: Arc<Mutex<Option<u64>>>, sender: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>>>> }
    impl InteractiveSession for LiveHost {
        fn session_id(&self) -> Option<String> { None }
        fn session_file(&self) -> Option<String> { None }
        fn cwd(&self) -> String { String::new() }
        fn is_reconnecting(&self) -> bool { false }
        fn is_fallback(&self) -> bool { false }
        fn remote_state(&self) -> Option<maho_interactive::interactive_host_runtime::RemoteSessionState> { None }
        fn subscribe_session_events(&self, listener: SessionEventListener) -> Option<SessionSubscription> { *self.listener.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(listener); None }
        fn request_remote_history(&self, generation: u64, sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>) {
            *self.generation.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(generation);
            *self.sender.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
        }
        fn new_session(&self, _parent_session: Option<String>) -> SessionFuture<'_, Result<ReplacementOutcome, String>> { Box::pin(async { Ok(ReplacementOutcome::Cancelled) }) }
        fn switch_session(&self, _session_path: String) -> SessionFuture<'_, Result<ReplacementOutcome, String>> { Box::pin(async { Ok(ReplacementOutcome::Cancelled) }) }
        fn fork(&self, _entry_id: String, _include_entry: bool) -> SessionFuture<'_, Result<ForkOutcome, String>> { Box::pin(async { Ok(ForkOutcome { outcome: ReplacementOutcome::Cancelled, editor_text: None }) }) }
        fn dispose(&self) -> SessionFuture<'_, ()> { Box::pin(async {}) }
    }

    let (mut mode, _directory) = native_mode();
    let listener = Arc::new(Mutex::new(None));
    let generation = Arc::new(Mutex::new(None));
    let sender = Arc::new(Mutex::new(None));
    mode.set_session_host(Arc::new(LiveHost { listener: listener.clone(), generation: generation.clone(), sender: sender.clone() }));
    let emit = listener.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone().expect("bridge subscribed");
    emit(&maho_ext_api::AgentSessionEvent::ContinuationError { error_message: "live-during-fetch".into() });
    let generation = generation.lock().unwrap_or_else(std::sync::PoisonError::into_inner).expect("request");
    sender.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().expect("sender").send((generation, Ok(vec![serde_json::json!({"role":"user","content":[{"type":"text","text":"remote-history"}],"timestamp":0})]))).expect("send history");
    mode.drain_events();
    let rendered = mode.render(80).join("\n");
    assert!(rendered.contains("remote-history"), "the snapshot is published: {rendered}");
    assert!(rendered.contains("live-during-fetch"), "a live event buffered during the fetch is applied after the snapshot: {rendered}");
}

#[tokio::test]
async fn mounting_a_host_disables_local_event_delivery() {
    use std::sync::{Arc, Mutex};
    use maho_interactive::interactive_host_runtime::{SessionEventListener, SessionSubscription};
    use maho_interactive::interactive_session::{ForkOutcome, InteractiveSession, ReplacementOutcome, SessionFuture};

    struct GateHost;
    impl InteractiveSession for GateHost {
        fn session_id(&self) -> Option<String> { None }
        fn session_file(&self) -> Option<String> { None }
        fn cwd(&self) -> String { String::new() }
        fn is_reconnecting(&self) -> bool { false }
        fn is_fallback(&self) -> bool { false }
        fn remote_state(&self) -> Option<maho_interactive::interactive_host_runtime::RemoteSessionState> { None }
        fn subscribe_session_events(&self, _listener: SessionEventListener) -> Option<SessionSubscription> { None }
        fn request_remote_history(&self, _generation: u64, _sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>) {}
        fn new_session(&self, _parent_session: Option<String>) -> SessionFuture<'_, Result<ReplacementOutcome, String>> { Box::pin(async { Ok(ReplacementOutcome::Cancelled) }) }
        fn switch_session(&self, _session_path: String) -> SessionFuture<'_, Result<ReplacementOutcome, String>> { Box::pin(async { Ok(ReplacementOutcome::Cancelled) }) }
        fn fork(&self, _entry_id: String, _include_entry: bool) -> SessionFuture<'_, Result<ForkOutcome, String>> { Box::pin(async { Ok(ForkOutcome { outcome: ReplacementOutcome::Cancelled, editor_text: None }) }) }
        fn dispose(&self) -> SessionFuture<'_, ()> { Box::pin(async {}) }
    }

    let (mut mode, _directory) = native_mode();
    assert!(mode.local_events_enabled(), "before mounting, the local session drives the view");
    mode.set_session_host(Arc::new(GateHost));
    assert!(!mode.local_events_enabled(), "a mounted host suppresses local session events");
}

#[tokio::test]
async fn mirrored_remote_state_drives_the_mode_reads() {
    use std::sync::Arc;
    use maho_interactive::interactive_host_runtime::{RemoteSessionState, SessionEventListener, SessionSubscription};
    use maho_interactive::interactive_session::{ForkOutcome, InteractiveSession, ReplacementOutcome, SessionFuture};

    struct StateHost;
    impl InteractiveSession for StateHost {
        fn session_id(&self) -> Option<String> { None }
        fn session_file(&self) -> Option<String> { None }
        fn cwd(&self) -> String { String::new() }
        fn is_reconnecting(&self) -> bool { false }
        fn is_fallback(&self) -> bool { false }
        fn remote_state(&self) -> Option<RemoteSessionState> {
            Some(RemoteSessionState { cwd: Some("/remote/cwd".into()), session_name: Some("remote-name".into()), is_streaming: true, auto_compaction_enabled: true, ..Default::default() })
        }
        fn subscribe_session_events(&self, _listener: SessionEventListener) -> Option<SessionSubscription> { None }
        fn request_remote_history(&self, _generation: u64, _sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>) {}
        fn new_session(&self, _parent_session: Option<String>) -> SessionFuture<'_, Result<ReplacementOutcome, String>> { Box::pin(async { Ok(ReplacementOutcome::Cancelled) }) }
        fn switch_session(&self, _session_path: String) -> SessionFuture<'_, Result<ReplacementOutcome, String>> { Box::pin(async { Ok(ReplacementOutcome::Cancelled) }) }
        fn fork(&self, _entry_id: String, _include_entry: bool) -> SessionFuture<'_, Result<ForkOutcome, String>> { Box::pin(async { Ok(ForkOutcome { outcome: ReplacementOutcome::Cancelled, editor_text: None }) }) }
        fn dispose(&self) -> SessionFuture<'_, ()> { Box::pin(async {}) }
    }

    let (mut mode, _directory) = native_mode();
    mode.set_session_host(Arc::new(StateHost));
    mode.submit("/session", Default::default()).await.expect("session info");
    let rendered = mode.render(80).join("\n");
    assert!(rendered.contains("remote-name"), "the mirrored session name drives the read: {rendered}");
}

#[tokio::test]
async fn turn_and_session_mutations_route_to_the_mounted_host() {
    use std::sync::{Arc, Mutex};
    use maho_interactive::interactive_host_runtime::{RemoteSessionState, SessionEventListener, SessionSubscription};
    use maho_interactive::interactive_session::{ForkOutcome, InteractiveSession, ReplacementOutcome, SessionFuture};

    struct RecordingHost { calls: Arc<Mutex<Vec<String>>> }
    impl InteractiveSession for RecordingHost {
        fn session_id(&self) -> Option<String> { None }
        fn session_file(&self) -> Option<String> { None }
        fn cwd(&self) -> String { String::new() }
        fn is_reconnecting(&self) -> bool { false }
        fn is_fallback(&self) -> bool { false }
        fn remote_state(&self) -> Option<RemoteSessionState> { Some(RemoteSessionState::default()) }
        fn subscribe_session_events(&self, _listener: SessionEventListener) -> Option<SessionSubscription> { None }
        fn request_remote_history(&self, _generation: u64, _sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>) {}
        fn request_remote_models(&self, _generation: u64, _sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>) {}
        fn request_remote_stats(&self, _generation: u64, _sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<serde_json::Value, String>)>) {}
        fn prompt(&self, _message: String, _options: maho_core::agent_session::PromptOptions) -> SessionFuture<'_, Result<(), String>> { let calls = self.calls.clone(); Box::pin(async move { calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push("prompt".into()); Ok(()) }) }
        fn abort(&self) -> SessionFuture<'_, Result<(), String>> { let calls = self.calls.clone(); Box::pin(async move { calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push("abort".into()); Ok(()) }) }
        fn steer(&self, _text: String) -> SessionFuture<'_, Result<(), String>> { let calls = self.calls.clone(); Box::pin(async move { calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push("steer".into()); Ok(()) }) }
        fn follow_up(&self, _text: String) -> SessionFuture<'_, Result<(), String>> { let calls = self.calls.clone(); Box::pin(async move { calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push("follow_up".into()); Ok(()) }) }
        fn compact(&self, _instructions: Option<String>) -> SessionFuture<'_, Result<(), String>> { Box::pin(async { Ok(()) }) }
        fn navigate_tree(&self, _entry_id: String, _options: maho_core::agent_session::TreeNavigationOptions) -> SessionFuture<'_, Result<maho_core::agent_session::AssistantEditResult, String>> { Box::pin(async { Ok(maho_core::agent_session::AssistantEditResult::default()) }) }
        fn edit_assistant_message(&self, _entry_id: String, _text: String, _options: maho_core::agent_session::TreeNavigationOptions) -> SessionFuture<'_, Result<maho_core::agent_session::AssistantEditResult, String>> { Box::pin(async { Ok(maho_core::agent_session::AssistantEditResult::default()) }) }
        fn reload(&self) -> SessionFuture<'_, Result<bool, String>> { Box::pin(async { Ok(true) }) }
        fn execute_bash(&self, _command: String, _exclude_from_context: bool) -> SessionFuture<'_, Result<serde_json::Value, String>> { Box::pin(async { Ok(serde_json::json!({})) }) }
        fn set_model(&self, _provider: String, _id: String) -> SessionFuture<'_, Result<(), String>> { let calls = self.calls.clone(); Box::pin(async move { calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push("set_model".into()); Ok(()) }) }
        fn set_session_name(&self, _name: String) -> SessionFuture<'_, Result<(), String>> { let calls = self.calls.clone(); Box::pin(async move { calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push("set_session_name".into()); Ok(()) }) }
        fn set_session_thinking_level(&self, _level: String) -> SessionFuture<'_, Result<(), String>> { Box::pin(async { Ok(()) }) }
        fn cycle_thinking_level(&self) -> SessionFuture<'_, Result<Option<String>, String>> { Box::pin(async { Ok(None) }) }
        fn cycle_model(&self, _forward: bool) -> SessionFuture<'_, Result<Option<String>, String>> { Box::pin(async { Ok(None) }) }
        fn export_jsonl(&self, _output_path: Option<String>) -> SessionFuture<'_, Result<Option<String>, String>> { Box::pin(async { Ok(None) }) }
        fn new_session(&self, _parent_session: Option<String>) -> SessionFuture<'_, Result<ReplacementOutcome, String>> { Box::pin(async { Ok(ReplacementOutcome::Cancelled) }) }
        fn switch_session(&self, _session_path: String) -> SessionFuture<'_, Result<ReplacementOutcome, String>> { Box::pin(async { Ok(ReplacementOutcome::Cancelled) }) }
        fn fork(&self, _entry_id: String, _include_entry: bool) -> SessionFuture<'_, Result<ForkOutcome, String>> { Box::pin(async { Ok(ForkOutcome { outcome: ReplacementOutcome::Cancelled, editor_text: None }) }) }
        fn dispose(&self) -> SessionFuture<'_, ()> { Box::pin(async {}) }
    }

    let (mut mode, _directory) = native_mode();
    let calls = Arc::new(Mutex::new(Vec::new()));
    mode.set_session_host(Arc::new(RecordingHost { calls: calls.clone() }));
    mode.submit("hello there", Default::default()).await.expect("prompt");
    mode.abort().await;
    mode.steer("steer text").await.expect("steer");
    mode.follow_up("follow text").await.expect("follow_up");
    mode.submit("/rename renamed", Default::default()).await.expect("rename");
    let recorded = calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert!(recorded.iter().any(|call| call == "prompt"), "prompt routes to the host: {recorded:?}");
    assert!(recorded.iter().any(|call| call == "abort"), "abort routes to the host: {recorded:?}");
    assert!(recorded.iter().any(|call| call == "steer"), "steer routes to the host: {recorded:?}");
    assert!(recorded.iter().any(|call| call == "follow_up"), "follow_up routes to the host: {recorded:?}");
    assert!(recorded.iter().any(|call| call == "set_session_name"), "/rename routes to the host: {recorded:?}");
}
