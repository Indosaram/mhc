//! Native user/assistant transcript slice of interactive-mode.ts.
//! Command dispatch, runtime replacement and extension UI remain partial.
use std::{cell::RefCell, collections::BTreeMap, rc::Rc, sync::Arc};
use maho_agent::types::AgentEvent;
use maho_core::agent_session::{AgentSession, AgentSessionSubscription, PromptDisposition, PromptOptions};
use maho_tui::tui::{Component, Container};
use crate::{components::{assistant_message::AssistantMessageComponent, user_message::UserMessageComponent, markdown_transform::get_markdown_theme}, theme::Theme};
use crate::components::{tool_execution::{ToolExecutionComponent, ToolExecutionOptions, ToolExecutionPresentation}, tool_execution_types::ToolExecutionResult};
use crate::components::{custom_editor::{CustomEditor, CustomEditorOptions}, extension_editor::editor_theme};

type ImageSubmissions = std::collections::VecDeque<(String, Vec<maho_ai::types::ImageContent>)>;
type CustomUiBuild = std::pin::Pin<Box<dyn std::future::Future<Output=Result<Box<dyn Component>, maho_ext_api::ExtensionFailure>>>>;

pub struct InteractiveMode {
    session: Arc<AgentSession>,
    events: tokio::sync::mpsc::UnboundedReceiver<maho_ext_api::AgentSessionEvent>,
    _subscription: AgentSessionSubscription,
    chat: Container,
    streaming: Option<Rc<RefCell<AssistantMessageComponent>>>,
    assistant_segments: BTreeMap<usize, Rc<RefCell<AssistantMessageComponent>>>,
    pending_tools: BTreeMap<String, Rc<RefCell<ToolExecutionComponent>>>,
    theme: Theme,
    pub editor: CustomEditor,
    submissions: Rc<RefCell<std::collections::VecDeque<String>>>,
    tree_copies: Rc<RefCell<std::collections::VecDeque<Option<String>>>>,
    rename_input: Option<crate::components::extension_input::ExtensionInputComponent>,
    rename_result: Rc<RefCell<Option<Option<String>>>>,
    shortcut_overlay: bool,
    last_clear_ms: Option<u64>,
    pub shutdown_requested: bool,
    pub agent_idle: bool,
    pub extension_ui: Arc<crate::interactive_extension_ui::InteractiveExtensionUi>,
    ui_requests: tokio::sync::mpsc::UnboundedReceiver<crate::interactive_extension_ui::UiRequest>,
    ui_dialog: Option<Box<dyn Component>>,
    ui_reply: Rc<RefCell<Option<tokio::sync::oneshot::Sender<Option<String>>>>>,
    header: Option<Box<dyn Component>>,
    footer: Option<Box<dyn Component>>,
    footer_data: Arc<maho_core::footer_data_provider::FooterDataProvider>,
    widgets: Vec<(String, Box<dyn Component>, maho_ext_api::WidgetPlacement)>,
    pub terminal_title: Option<String>,
    markdown_transformers: Vec<crate::components::markdown_transform::MarkdownTransformer>,
    reveal: crate::streaming_reveal::StreamingRevealController,
    clock: std::time::Instant,
    tool_reveal: crate::tool_result_reveal::ToolResultRevealController,
    tool_args_reveal: crate::tool_args_reveal::ToolArgsRevealController,
    tool_partial_json: BTreeMap<String, String>,
    last_status: Option<(usize, Rc<RefCell<maho_tui::components::text::Text>>)>,
    assistant_cards: Vec<Rc<RefCell<AssistantMessageComponent>>>,
    tool_cards: Vec<Rc<RefCell<ToolExecutionComponent>>>,
    pub tools_expanded: bool,
    local_dialog_reply: Option<tokio::sync::oneshot::Receiver<Option<String>>>,
    working_started_ms: Option<f64>,
    working_message: Option<String>,
    working_visible: bool,
    editor_host: Rc<dyn maho_tui::components::editor::EditorTuiHost>,
    hidden_thinking_label: String,
    history_expansion: Vec<Box<dyn FnMut(bool)>>,
    question: Option<crate::components::ask_user_question::AskUserQuestionComponent>,
    async_question_widget: Option<crate::components::ask_user_async_widget::AskUserAsyncWidget>,
    expanded_question_widget: Option<crate::components::ask_user_async_widget::AskUserAsyncWidget>,
    queued_questions: std::collections::VecDeque<crate::interactive_extension_ui::UiRequest>,
    question_reply: Rc<RefCell<Option<tokio::sync::oneshot::Sender<maho_ext_api::QuestionResponse>>>>,
    pending_images: Rc<RefCell<BTreeMap<u64, maho_ai::types::ImageContent>>>,
    submission_images: Rc<RefCell<ImageSubmissions>>,
    working_indicator: Option<maho_ext_api::WorkingIndicatorOptions>,
    custom_editor: Option<Box<dyn maho_tui::editor_component::EditorComponent>>,
    custom_ui_builds: Vec<CustomUiBuild>,
    custom_ui_result: Rc<RefCell<Option<serde_json::Value>>>,
    custom_ui_reply: Option<tokio::sync::oneshot::Sender<Result<serde_json::Value, maho_ext_api::ExtensionFailure>>>,
    mounted_renderer: Option<std::rc::Weak<RefCell<crate::tui_renderer::InteractiveTui>>>,
    terminal_dimensions: Rc<std::cell::Cell<(u16,u16)>>,
    custom_overlay: Option<maho_tui::tui::OverlayHandle>,
}

impl InteractiveMode {
    pub(crate) fn tick_now(&mut self) { self.tick(self.clock.elapsed().as_secs_f64()*1000.0); }
    pub(crate) fn set_terminal_dimensions(&self, columns:usize, rows:usize) { self.terminal_dimensions.set((u16::try_from(columns).unwrap_or(u16::MAX),u16::try_from(rows).unwrap_or(u16::MAX))); }
    pub(crate) fn set_mounted_renderer(&mut self, renderer: Rc<RefCell<crate::tui_renderer::InteractiveTui>>) { self.mounted_renderer = Some(Rc::downgrade(&renderer)); }
    pub(crate) fn terminal_settings(&self) -> (bool, bool, bool) {
        self.session.with_settings_manager(|settings| (settings.get_bool("showHardwareCursor").unwrap_or(false), settings.get_bool("clearOnShrink").unwrap_or(false), settings.get_bool("showTerminalProgress").unwrap_or(true)))
    }
    fn keybindings(&self) -> maho_tui::keybindings::KeybindingsManager {
        maho_core::keybindings::KeybindingsManager::create(Some(&self.session.agent_dir())).inner().clone()
    }
    fn output_pad(&self) -> usize { self.session.with_settings_manager(|settings| settings.get_number("outputPad").unwrap_or(1.0) as usize) }
    pub fn new(session: Arc<AgentSession>, theme: Theme, host: Rc<dyn maho_tui::components::editor::EditorTuiHost>) -> Self {
        let (sender, events) = tokio::sync::mpsc::unbounded_channel();
        let subscription = session.subscribe(Arc::new(move |event| { drop(sender.send(event.clone())); }));
        let submissions = Rc::new(RefCell::new(std::collections::VecDeque::new()));
        let captured = submissions.clone();
        let keys = Arc::new(maho_core::keybindings::KeybindingsManager::create(Some(&session.agent_dir())).inner().clone());
        let mut editor = CustomEditor::new(host.clone(), editor_theme(&theme), keys, CustomEditorOptions::default());
        let (padding, max_visible) = session.with_settings_manager(|settings| (settings.get_number("editorPaddingX").unwrap_or(0.0), settings.get_number("autocompleteMaxVisible").unwrap_or(10.0)));
        editor.set_padding_x(padding as usize); editor.editor.set_autocomplete_max_visible(max_visible as usize);
        Self::setup_autocomplete(&session, &mut editor);
        let pending_images = Rc::new(RefCell::new(BTreeMap::<u64, maho_ai::types::ImageContent>::new()));
        let submission_images = Rc::new(RefCell::new(std::collections::VecDeque::new()));
        let images = pending_images.clone();
        editor.editor.on_image_markers_changed = Some(Box::new(move |order| {
            let previous = std::mem::take(&mut *images.borrow_mut());
            images.borrow_mut().extend(order.iter().enumerate().filter_map(|(index, id)| {
                previous.get(id).cloned().map(|image| (u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1), image))
            }));
        }));
        let images = pending_images.clone();
        editor.editor.snapshot_attachment_state = Some(Box::new(move || Some(Rc::new(images.borrow().clone()))));
        let images = pending_images.clone();
        editor.editor.restore_attachment_state = Some(Box::new(move |snapshot| {
            if let Some(snapshot) = snapshot.downcast_ref::<BTreeMap<u64, maho_ai::types::ImageContent>>() { *images.borrow_mut() = snapshot.clone(); }
        }));
        let images = pending_images.clone();
        let queued_images = submission_images.clone();
        editor.editor.on_submit = Some(Box::new(move |text| {
            if !text.trim().is_empty() {
                let mut payloads = std::mem::take(&mut *images.borrow_mut());
                let ordered = maho_tui::image_markers::IMAGE_MARKER_REGEX.captures_iter(text)
                    .filter_map(|capture| capture.get(1).and_then(|id| id.as_str().parse::<u64>().ok()))
                    .filter_map(|id| payloads.remove(&id)).collect();
                queued_images.borrow_mut().push_back((text.trim().to_owned(), ordered));
                captured.borrow_mut().push_back(text.trim().into());
            }
        }));
        let (extension_ui, ui_requests) = crate::interactive_extension_ui::InteractiveExtensionUi::channel(maho_ext_api::Theme { name: Some(theme.name.clone()), colors: theme.resolved_colors(), ..Default::default() });
        *extension_ui.theme_directory.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = std::path::Path::new(&session.agent_dir()).join("themes");
        let (smooth, fps, hide) = session.with_settings_manager(|settings| (settings.get_bool("smoothStreaming").unwrap_or(true), settings.get_number("smoothStreamingFps").unwrap_or(60.0), settings.get_bool("hideThinkingBlock").unwrap_or(false)));
        Self { footer_data:Arc::new(maho_core::footer_data_provider::FooterDataProvider::new(&session.cwd())), tree_copies:Default::default(), expanded_question_widget:None, queued_questions:Default::default(), terminal_dimensions:Rc::new(std::cell::Cell::new((80,u16::try_from(host.terminal_rows()).unwrap_or(u16::MAX)))), mounted_renderer:None, custom_overlay:None, custom_ui_builds:Vec::new(), custom_ui_result:Rc::new(RefCell::new(None)), custom_ui_reply:None, working_indicator:None, custom_editor:None, pending_images, submission_images, session, events, _subscription: subscription, chat: Container::new(), streaming: None, assistant_segments: BTreeMap::new(), pending_tools: BTreeMap::new(), theme, editor, submissions, rename_input: None, rename_result: Rc::new(RefCell::new(None)), shortcut_overlay: false, last_clear_ms: None, shutdown_requested: false, agent_idle: true, extension_ui, ui_requests, ui_dialog: None, ui_reply: Rc::new(RefCell::new(None)), header: None, footer: None, widgets: Vec::new(), terminal_title: None, markdown_transformers: Vec::new(), reveal: crate::streaming_reveal::StreamingRevealController::new(smooth, fps, hide), clock: std::time::Instant::now(), tool_reveal: crate::tool_result_reveal::ToolResultRevealController::new(smooth, fps), tool_args_reveal: crate::tool_args_reveal::ToolArgsRevealController::new(smooth, fps), tool_partial_json: BTreeMap::new(), last_status: None, assistant_cards: Vec::new(), tool_cards: Vec::new(), tools_expanded: false, local_dialog_reply: None, working_started_ms: None, working_message: None, working_visible: true, editor_host: host, hidden_thinking_label:"Thinking...".into(), history_expansion: Vec::new(), question:None, async_question_widget:None, question_reply:Rc::new(RefCell::new(None)) }
    }

    pub fn use_registered_markdown_transformers(&mut self, extensions: &[maho_ext_api::LoadedExtension]) {
        self.markdown_transformers = extensions.iter().filter_map(|extension| extension.markdown_transformer.clone()).map(|transformer| -> crate::components::markdown_transform::MarkdownTransformer {
            Rc::new(move |text, context| {
                use crate::components::markdown_transform::MessageType;
                let message_type = match context.message_type { MessageType::User => maho_ext_api::MarkdownMessageType::User, MessageType::Assistant => maho_ext_api::MarkdownMessageType::Assistant, MessageType::AssistantThinking => maho_ext_api::MarkdownMessageType::AssistantThinking };
                Ok(Some(transformer(text, &maho_ext_api::MarkdownTransformContext { message_type, is_streaming: context.is_streaming, available_width: context.available_width })))
            })
        }).collect();
    }

    fn setup_autocomplete(session: &AgentSession, editor: &mut CustomEditor) {
        let mut commands: Vec<_> = maho_core::slash_commands::builtin_slash_commands().into_iter().map(|command| maho_tui::autocomplete::CommandSpec::Command { name:command.name.into(), description:Some(command.description.into()), argument_hint:command.argument_hint.map(str::to_owned), get_argument_completions:None }).collect();
        commands.extend(session.prompt_templates().into_iter().map(|template| maho_tui::autocomplete::CommandSpec::Command { name:template.name, description:Some(template.description), argument_hint:template.argument_hint, get_argument_completions:None }));
        editor.editor.cancel_autocomplete();
        editor.editor.set_autocomplete_provider(Rc::new(RefCell::new(maho_tui::autocomplete::CombinedAutocompleteProvider::new(commands, &session.cwd(), None))));
    }

    pub fn rebuild_history(&mut self) {
        self.chat.clear(); self.pending_tools.clear(); self.streaming = None; self.assistant_segments.clear();
        self.tool_partial_json.clear(); self.tool_args_reveal.stop(); self.tool_reveal.stop(); self.reveal.stop();
        self.assistant_cards.clear(); self.tool_cards.clear(); self.last_status = None;
        self.history_expansion.clear();
        for message in self.session.messages() { self.add_history_message(&message); }
    }

    pub fn add_history_message(&mut self, message: &maho_agent::types::AgentMessage) {
        use maho_agent::types::{AgentMessage, CustomAgentMessage};
        match message {
            AgentMessage::Llm(maho_ai::types::Message::Assistant(message)) => crate::replay_assistant_tools::replay_assistant_tools(message, self),
            AgentMessage::Llm(maho_ai::types::Message::ToolResult(message)) => {
                if let Some(component) = self.pending_tools.remove(&message.tool_call_id) {
                    let value = serde_json::to_value(message).expect("tool result");
                    component.borrow_mut().update_result(Self::tool_result(&value, message.is_error), false);
                }
            }
            AgentMessage::Llm(maho_ai::types::Message::User(_)) => {
                self.handle_event(&AgentEvent::MessageStart { message: message.clone() });
                let value = serde_json::to_value(message).expect("user message");
                let text = value["content"].as_str().map(str::to_owned).unwrap_or_else(|| value["content"].as_array().map(|parts| parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default());
                self.editor.editor.add_to_history(&text);
            }
            AgentMessage::Custom(CustomAgentMessage::Custom(message)) if message.display => {
                let component = Rc::new(RefCell::new(crate::components::custom_message::CustomMessageComponent::new(serde_json::to_value(message).expect("custom message"), None, self.theme.clone(), get_markdown_theme(&self.theme), self.output_pad())));
                component.borrow_mut().set_expanded(self.tools_expanded); self.chat.add_child(component.clone());
                self.history_expansion.push(Box::new(move |expanded| component.borrow_mut().set_expanded(expanded)));
            }
            AgentMessage::Custom(CustomAgentMessage::BranchSummary(message)) => {
                let component = Rc::new(RefCell::new(crate::components::branch_summary_message::BranchSummaryMessageComponent::new(message.summary.clone(), self.theme.clone(), get_markdown_theme(&self.theme), crate::components::keybinding_hints::key_display_text("app.tools.expand"))));
                component.borrow_mut().set_expanded(self.tools_expanded); self.chat.add_child(component.clone());
                self.history_expansion.push(Box::new(move |expanded| component.borrow_mut().set_expanded(expanded)));
            }
            AgentMessage::Custom(CustomAgentMessage::CompactionSummary(message)) => {
                let component = Rc::new(RefCell::new(crate::components::compaction_summary_message::CompactionSummaryMessageComponent::new(serde_json::to_value(message).expect("summary"), self.theme.clone(), get_markdown_theme(&self.theme), crate::components::keybinding_hints::key_display_text("app.tools.expand"))));
                component.borrow_mut().set_expanded(self.tools_expanded); self.chat.add_child(component.clone());
                self.history_expansion.push(Box::new(move |expanded| component.borrow_mut().set_expanded(expanded)));
            }
            AgentMessage::Custom(CustomAgentMessage::BashExecution(message)) => {
                let mut component = crate::components::bash_execution::BashExecutionComponent::new(&message.command, message.exclude_from_context.unwrap_or(false), self.theme.clone());
                component.append_output(&message.output);
                component.set_complete(message.exit_code.map(|code| i32::try_from(code).expect("exit code")), message.cancelled, None, message.full_output_path.clone());
                component.set_expanded(self.tools_expanded);
                let component = Rc::new(RefCell::new(component)); self.chat.add_child(component.clone());
                self.history_expansion.push(Box::new(move |expanded| component.borrow_mut().set_expanded(expanded)));
            }
            _ => {}
        }
    }

    pub async fn bind_extensions(&mut self) {
        let session = self.session.clone();
        let bindings = maho_core::agent_session::ExtensionBindings { ui_context: Some(self.extension_ui.clone()), mode: Some(maho_ext_api::ExtensionMode::Tui), ..Default::default() };
        let binding = session.bind_extensions(bindings);
        tokio::pin!(binding);
        loop { tokio::select! { () = &mut binding => break, Some(request) = self.ui_requests.recv() => self.handle_ui_request(request) } }
        self.drain_ui_requests();
    }

    pub fn handle_input_at(&mut self, data: &str, now_ms: u64) {
        if let Some(data) = self.filter_terminal_input(data) { self.handle_filtered_input_at(&data, now_ms); }
    }

    fn filter_terminal_input(&self, data: &str) -> Option<String> {
        let listeners = self.extension_ui.terminal_input.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let mut data = data.to_owned();
        for listener in listeners {
            if let Some(result) = listener(&data) {
                if result.consume.unwrap_or(false) { return None; }
                if let Some(replacement) = result.data { data = replacement; }
            }
        }
        Some(data)
    }

    fn handle_filtered_input_at(&mut self, data: &str, now_ms: u64) {
        if self.async_question_widget.is_some() {
            let keys = self.keybindings();
            if self.ui_dialog.is_none() && self.rename_input.is_none() && crate::components::ask_user_answer_key::matches_ask_user_answer_key(data, &maho_core::keybindings::host_platform(), &keys) { self.expanded_question_widget = self.async_question_widget.take(); return; }
        }
        if self.expanded_question_widget.is_some() && let Some(question) = &self.question
            && ((self.keybindings().matches(data,"tui.select.cancel") && question.state.focus != crate::components::ask_user_question_state::QuestionFocus::OwnAnswer) || maho_tui::keys::matches_key(data,"ctrl+c")) {
            self.async_question_widget = self.expanded_question_widget.take(); return;
        }
        if self.async_question_widget.is_none() && let Some(question) = &mut self.question { question.handle_input(data); if self.question_reply.borrow().is_none() { self.question = None; } return; }
        if self.ui_dialog.is_some() || self.rename_input.is_some() { self.handle_editor_input(data); return; }
        if self.shortcut_overlay { self.shortcut_overlay = false; return; }
        let keys = self.keybindings();
        if keys.matches(data, "app.model.select") { if let Err(error) = self.dispatch_command("/model") { self.show_status(error); } return; }
        for (action, command) in [("app.session.tree", "/tree"), ("app.session.fork", "/fork"), ("app.session.renameCurrent", "/rename")] {
            if keys.matches(data, action) { if let Err(error) = self.dispatch_command(command) { self.show_status(error); } return; }
        }
        if keys.matches(data, "app.session.new") { self.submissions.borrow_mut().push_back("/new".into()); return; }
        if keys.matches(data, "app.clear") {
            if self.last_clear_ms.is_some_and(|last| now_ms.saturating_sub(last) < 500) { self.shutdown_requested = true; }
            else { self.editor.editor.set_text(""); self.last_clear_ms = Some(now_ms); }
            return;
        }
        if keys.matches(data, "app.exit") && self.editor.editor.get_text().is_empty() { self.shutdown_requested = true; return; }
        if keys.matches(data, "app.thinking.cycle") {
            if self.session.cycle_thinking_level().is_none() { self.show_status("Current model does not support thinking".into()); }
            return;
        }
        if keys.matches(data, "app.message.dequeue") { self.restore_queued_messages(false); return; }
        if keys.matches(data, "app.tools.expand") { self.set_tools_expanded(!self.tools_expanded); return; }
        if keys.matches(data, "app.thinking.toggle") {
            self.reveal.hide_thinking = !self.reveal.hide_thinking;
            let hidden = self.reveal.hide_thinking;
            if let Some(component) = &self.streaming { let content = self.reveal.resync_visibility(self.clock.elapsed().as_secs_f64() * 1000.0); component.borrow_mut().update_content(&content, Some(true)); }
            if let Err(error) = self.session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &[("hideThinkingBlock".into(), serde_json::json!(hidden))].into_iter().collect())) { self.show_status(error); }
            for component in &self.assistant_cards { component.borrow_mut().set_hide_thinking_block(self.reveal.hide_thinking); }
            self.show_status(format!("Thinking blocks: {}", if hidden { "hidden" } else { "visible" }));
            return;
        }
        if data == "?" && self.editor.editor.get_text().is_empty() { self.shortcut_overlay = true; return; }
        self.handle_editor_input(data);
    }

    pub async fn submit_editor(&mut self) -> Result<Option<PromptDisposition>, String> {
        loop {
            let copy=self.tree_copies.borrow_mut().pop_front();
            let Some(copy)=copy else {break;};
            if let Some(text)=copy.filter(|text|!text.is_empty()) {
                match crate::interactive_clipboard::copy(&text).await {
                    Ok(sequence)=>{if let Some(sequence)=sequence {maho_tui::process_stdio::stdout_write(&sequence);}self.show_status("Copied selected message to clipboard".into());}
                    Err(error)=>self.show_status(error),
                }
            }else{self.show_status("Selected entry has no text to copy".into());}
        }
        let text = self.submissions.borrow_mut().pop_front();
        let Some(text) = text else { return Ok(None); };
        self.editor.editor.add_to_history(&text);
        let images = {
            let mut queued = self.submission_images.borrow_mut();
            queued.iter().position(|(submitted, _)| submitted == &text).and_then(|index| queued.remove(index)).map(|(_, images)| images)
        };
        let options = PromptOptions { images, streaming_behavior: Some(maho_ext_api::StreamingBehavior::Steer), ..Default::default() };
        self.submit(&text, options).await.map(Some)
    }

    /// Pair an editor marker with its in-memory image payload.
    pub fn attach_image(&mut self, image: maho_ai::types::ImageContent) {
        let id = self.editor.editor.insert_image_marker();
        self.pending_images.borrow_mut().insert(id, image);
    }

    pub async fn handle_runtime_input(&mut self, data: &str, now_ms: u64) -> Result<(), String> {
        let Some(data) = self.filter_terminal_input(data) else { return Ok(()); };
        let data = data.as_str();
        if self.ui_dialog.is_some() || self.rename_input.is_some() || (self.question.is_some() && self.async_question_widget.is_none()) { self.handle_filtered_input_at(data, now_ms); return Ok(()); }
        let keys = self.keybindings();
        if keys.matches(data, "app.model.cycleForward") || keys.matches(data, "app.model.cycleBackward") {
            if let Some(result) = self.session.cycle_model(keys.matches(data, "app.model.cycleForward")).await? { self.show_status(format!("Switched to {}", result.model.name)); }
            else { self.show_status("No other models available for cycling".into()); }
            return Ok(());
        }
        if keys.matches(data,"app.message.copy") { self.submit("/copy",Default::default()).await?;return Ok(()); }
        if keys.matches(data,"app.clipboard.pasteImage") {
            if let Some(text)=crate::interactive_clipboard::read_text().await {
                if let Some(editor)=&mut self.custom_editor {editor.insert_text_at_cursor(&text);}else{self.editor.editor.insert_text_at_cursor(&text);}
            }
            return Ok(());
        }
        if keys.matches(data, "app.interrupt") && !self.agent_idle { self.abort_and_restore_queue().await; return Ok(()); }
        self.handle_filtered_input_at(data, now_ms);
        Ok(())
    }

    pub fn set_tools_expanded(&mut self, expanded: bool) {
        self.extension_ui.tools_expanded.store(expanded, std::sync::atomic::Ordering::Relaxed);
        if self.tools_expanded == expanded { return; }
        self.tools_expanded = expanded;
        for component in &self.tool_cards { component.borrow_mut().set_expanded(expanded); }
        for component in &self.assistant_cards { component.borrow_mut().set_expanded(expanded); }
        for update in &mut self.history_expansion { update(expanded); }
        self.show_status(format!("Tool output: {}", if expanded { "expanded" } else { "collapsed" }));
    }

    pub async fn submit(&mut self, text: &str, options: PromptOptions) -> Result<PromptDisposition, String> {
        if text.trim() == "/copy" {
            if let Some(text) = self.session.get_last_assistant_text().filter(|text|!text.is_empty()) {
                match crate::interactive_clipboard::copy(&text).await {
                    Ok(sequence) => {
                        if let Some(sequence) = sequence { maho_tui::process_stdio::stdout_write(&sequence); }
                        self.show_status("Copied last agent message to clipboard".into());
                    }
                    Err(error) => return Err(error),
                }
            } else { return Err("No agent messages to copy yet.".into()); }
            return Ok(PromptDisposition::Handled);
        }
        if let Some(command) = text.strip_prefix('!') {
            let (command, excluded) = command.strip_prefix('!').map_or((command, false), |command| (command, true));
            let component = Rc::new(RefCell::new(crate::components::bash_execution::BashExecutionComponent::new(command, excluded, self.theme.clone())));
            component.borrow_mut().set_expanded(self.tools_expanded); self.chat.add_child(component.clone());
            let session = self.session.clone();
            let execution = session.execute_bash(command, None, excluded, None, None); tokio::pin!(execution);
            let result = loop { tokio::select! {
                result = &mut execution => break result,
                Some(event) = self.events.recv() => { if let maho_ext_api::AgentSessionEvent::BashExecutionUpdate { delta, .. } = &event { component.borrow_mut().append_output(delta); } else { self.handle_session_event(&event); } },
                Some(request) = self.ui_requests.recv() => self.handle_ui_request(request),
            }};
            while let Ok(event) = self.events.try_recv() { if let maho_ext_api::AgentSessionEvent::BashExecutionUpdate { delta, .. } = &event { component.borrow_mut().append_output(delta); } else { self.handle_session_event(&event); } }
            let result = match result { Ok(result) => result, Err(error) => { component.borrow_mut().append_output(&error); component.borrow_mut().set_complete(Some(1), false, None, None); return Err(error); } };
            component.borrow_mut().set_complete(result.exit_code, result.cancelled, None, result.full_output_path.map(|path| path.to_string_lossy().into_owned()));
            self.history_expansion.push(Box::new(move |expanded| component.borrow_mut().set_expanded(expanded)));
            return Ok(PromptDisposition::Handled);
        }
        if text.trim() == "/reload" {
            if self.session.reload().await? {
                let (padding, max_visible) = self.session.with_settings_manager(|settings| (settings.get_number("editorPaddingX").unwrap_or(0.0), settings.get_number("autocompleteMaxVisible").unwrap_or(10.0)));
                self.editor.set_padding_x(padding as usize); self.editor.editor.set_autocomplete_max_visible(max_visible as usize);
                Self::setup_autocomplete(&self.session, &mut self.editor); self.rebuild_history(); self.show_status("Reloaded session resources".into());
            }
            return Ok(PromptDisposition::Handled);
        }
        if text.trim() == "/compact" || text.trim().starts_with("/compact ") {
            let session = self.session.clone();
            let compact = session.compact(text.trim().strip_prefix("/compact "));
            tokio::pin!(compact);
            let result = loop { tokio::select! {
                result = &mut compact => break result,
                Some(request) = self.ui_requests.recv() => self.handle_ui_request(request),
                Some(event) = self.events.recv() => self.handle_session_event(&event),
            }}?;
            self.rebuild_history();
            self.show_status(format!("Compacted from {} tokens", result.tokens_before));
            return Ok(PromptDisposition::Handled);
        }
        if text.trim() == "/new" {
            if self.session.new_session(None).await? { self.rebuild_history(); self.show_status("Started new session".into()); }
            return Ok(PromptDisposition::Handled);
        }
        if text.trim() == "/clone" {
            let leaf = self.session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned));
            if let Some(leaf) = leaf {
                let result = self.session.fork(&leaf, true).await?;
                if !result.cancelled { self.rebuild_history(); self.editor.editor.set_text(""); self.show_status("Cloned to new session".into()); }
            } else { self.show_status("Nothing to clone yet".into()); }
            return Ok(PromptDisposition::Handled);
        }
        if text.trim().starts_with("/resume ") {
            let path = get_path_command_argument(text.trim(), "/resume").ok_or("Missing session path")?;
            if self.session.switch_session(&path).await? { self.rebuild_history(); self.editor.editor.set_text(""); self.show_status("Resumed session".into()); }
            return Ok(PromptDisposition::Handled);
        }
        if let Some(id) = text.trim().strip_prefix("/fork ") {
            let result = self.session.fork(id.trim(), false).await?;
            if !result.cancelled { self.rebuild_history(); self.editor.editor.set_text(result.editor_text.as_deref().unwrap_or("")); self.show_status("Forked to new session".into()); }
            return Ok(PromptDisposition::Handled);
        }
        if let Some(id) = text.trim().strip_prefix("/tree ") {
            if self.session.with_session_manager(|manager| manager.leaf_id().is_some_and(|leaf| leaf == id.trim())) {
                self.show_status("Already at this point".into()); return Ok(PromptDisposition::Handled);
            }
            if self.session.is_streaming() { self.restore_queued_messages(false); self.session.abort().await; }
            if self.session.is_compacting() { return Err("Wait for the current compaction or tree navigation to finish before navigating the session tree.".into()); }
            let result = self.session.navigate_tree(id.trim(), Default::default()).await?;
            if !result.cancelled && result.aborted != Some(true) { self.rebuild_history(); if let Some(text) = result.editor_text { self.editor.editor.set_text(&text); } self.show_status("Navigated to selected point".into()); }
            return Ok(PromptDisposition::Handled);
        }
        if let Some(reference) = text.trim().strip_prefix("/model ") {
            let (provider, id) = reference.trim().split_once('/').ok_or("Model reference requires provider/model")?;
            let model = self.session.model_registry().find(provider, id).ok_or_else(|| format!("Model not found: {reference}"))?;
            let requested = model.clone();
            self.session.set_model(model).await?;
            if maho_ai::models::models_are_equal(Some(&self.session.model()), Some(&requested)) { self.show_status(format!("Switched to {}", self.session.model().name)); }
            else { self.show_status(format!("Model switch pending: {reference}")); }
            return Ok(PromptDisposition::Handled);
        }
        if self.dispatch_command(text)? { return Ok(PromptDisposition::Handled); }
        let session = self.session.clone();
        let prompt = session.prompt(text, options);
        tokio::pin!(prompt);
        let result = loop {
            tokio::select! {
                result = &mut prompt => break result,
                Some(event) = self.events.recv() => {
                    self.handle_session_event(&event);
                }
                Some(request) = self.ui_requests.recv() => self.handle_ui_request(request),
            }
        };
        self.drain_events();
        result
    }

    pub async fn abort(&self) { self.session.abort().await; }

    pub fn restore_queued_messages(&mut self, abort_will_follow: bool) -> usize {
        let cleared = self.session.clear_queue(abort_will_follow);
        let queued = cleared.ordered.iter().map(|message| message.text.as_str()).collect::<Vec<_>>();
        let count = queued.len();
        if count > 0 {
            let current = self.editor.editor.get_text();
            let queued = queued.join("\n\n");
            self.editor.editor.set_text(&[queued.as_str(), current.as_str()].into_iter().filter(|text| !text.trim().is_empty()).collect::<Vec<_>>().join("\n\n"));
        }
        self.show_status(if count == 0 { "No queued messages to restore".into() } else { format!("Restored {count} queued message{} to editor", if count > 1 { "s" } else { "" }) });
        count
    }

    pub async fn abort_and_restore_queue(&mut self) -> usize {
        let queued = self.session.clear_queue(true).ordered;
        self.session.abort().await;
        if !queued.is_empty() {
            let text = queued.iter().map(|message| message.text.as_str()).collect::<Vec<_>>().join("\n\n");
            let current = self.editor.editor.get_text();
            self.editor.editor.set_text(&[text.as_str(), current.as_str()].into_iter().filter(|text| !text.trim().is_empty()).collect::<Vec<_>>().join("\n\n"));
        }
        queued.len()
    }

    pub async fn steer(&self, text: &str) -> Result<(), String> { self.session.steer(text, None, Default::default()).await }

    pub async fn follow_up(&self, text: &str) -> Result<(), String> { self.session.follow_up(text, None, Default::default()).await }

    fn show_status(&mut self, text: String) {
        let text = self.theme.fg(crate::theme::ThemeColor::Dim, &text);
        if let Some((count, component)) = &self.last_status && *count == self.chat.children.len() { component.borrow_mut().set_text(text); return; }
        self.chat.add_child(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1))));
        let component = Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding(text, 1, 0)));
        self.chat.add_child(component.clone());
        self.last_status = Some((self.chat.children.len(), component));
    }

    fn dispatch_command(&mut self, text: &str) -> Result<bool, String> {
        let text = text.trim();
        if text == "/changelog" {
            let source=maho_core::changelog_source::resolve_changelog_source();
            let entries=crate::interactive_changelog::entries(&source.path);
            let markdown=if entries.is_empty(){"No changelog entries found.".into()}else{entries.into_iter().rev().map(|(version,content)|if source.rewrite_links {crate::interactive_changelog::normalize_links(&content,&version)}else{content}).collect::<Vec<_>>().join("\n\n")};
            self.chat.add_child(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1))));
            self.chat.add_child(Rc::new(RefCell::new(crate::components::dynamic_border::DynamicBorder::new(self.theme.clone()))));
            self.chat.add_child(Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding(self.theme.bold(&self.theme.fg(crate::theme::ThemeColor::Accent,"What's New")),1,0))));
            self.chat.add_child(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1))));
            self.chat.add_child(Rc::new(RefCell::new(crate::components::markdown_transform::MarkdownComponent(maho_tui::components::markdown::Markdown::new(&markdown,1,1,get_markdown_theme(&self.theme),None,Default::default())))));
            self.chat.add_child(Rc::new(RefCell::new(crate::components::dynamic_border::DynamicBorder::new(self.theme.clone()))));
            return Ok(true);
        }
        if text == "/resume" {
            use crate::components::session_selector::SessionSelectorComponent;
            let (directory, default_directory) = self.session.with_session_manager(|manager| (manager.session_dir().to_owned(), manager.uses_default_session_dir()));
            let current_directory = directory.clone();
            let all_directory = if default_directory { std::path::Path::new(&self.session.agent_dir()).join("sessions") } else { std::path::PathBuf::from(directory) };
            let convert = |info: maho_core::session_discovery::SessionInfo| crate::components::session_selector_search::SessionInfo { path:info.path, id:info.id, cwd:info.cwd, name:info.name, parent_session_path:info.parent_session_path, modified_ms:info.modified.timestamp_millis(), message_count:info.message_count, first_message:info.first_message, all_messages_text:info.all_messages_text };
            let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
            let selected = self.ui_reply.clone(); let cancelled = selected.clone(); let exited = selected.clone(); let submissions = self.submissions.clone(); let exit_submissions = self.submissions.clone(); let host = self.editor_host.clone();
            self.ui_dialog = Some(Box::new(SessionSelectorComponent::new(&self.theme, Arc::new(self.keybindings()),
                Box::new(move |_| maho_core::session_discovery::list_sessions_from_dir(&current_directory, None, 0, None).into_iter().map(convert).collect()),
                Box::new(move |_| {
                    let mut sessions = maho_core::session_discovery::list_sessions_from_dir(&all_directory.to_string_lossy(), None, 0, None);
                    if default_directory && let Ok(directories) = std::fs::read_dir(&all_directory) {
                        for directory in directories.flatten().filter(|entry| entry.path().is_dir()) { sessions.extend(maho_core::session_discovery::list_sessions_from_dir(&directory.path().to_string_lossy(), None, 0, None)); }
                    }
                    sessions.into_iter().map(convert).collect()
                }),
                Box::new(move |path| { submissions.borrow_mut().push_back(format!("/resume \"{path}\"")); selected.borrow_mut().take(); }),
                Box::new(move || { cancelled.borrow_mut().take(); }), Box::new(move || { exit_submissions.borrow_mut().push_back("/quit".into()); exited.borrow_mut().take(); }),
                Box::new(move || host.request_render()), Some(Box::new(|path, name| { let mut manager = maho_core::session_manager::SessionManager::open(path, None, None, None); manager.append_session_info(name); })), Some(true),
                self.session.session_file().as_deref(), std::env::var("HOME").ok())));
            return Ok(true);
        }
        if text == "/settings" {
            use crate::components::settings_selector::{SettingsSelectorComponent, SettingsConfig, SettingsCallbacks, ThinkingLevel};
            let mut config = SettingsConfig { auto_compact:self.session.auto_compaction_enabled(),
                thinking_level:ThinkingLevel::from_name(self.session.thinking_level().as_str()).expect("session thinking level"),
                available_thinking_levels:self.session.get_available_thinking_levels().into_iter().filter_map(|level| ThinkingLevel::from_name(level.as_str())).collect(), ..Default::default() };
            let theme_registry = crate::theme::registry::ThemeRegistry::new(std::path::Path::new(&self.session.agent_dir()).join("themes"), &self.theme.name, self.theme.get_color_mode()).map_err(|error| error.to_string())?;
            config.current_theme = self.theme.name.clone(); config.available_themes = theme_registry.get_available_themes_with_paths().into_iter().map(|theme| theme.name).collect();
            self.session.with_settings_manager(|settings| {
                macro_rules! boolean { ($field:ident, $key:literal) => { config.$field = settings.get_bool($key).unwrap_or(config.$field); }; }
                macro_rules! number { ($field:ident, $key:literal) => { if let Some(value) = settings.get_number($key) { config.$field = value as _; } }; }
                macro_rules! string { ($field:ident, $key:literal) => { if let Some(value) = settings.get_string($key) { config.$field = value; } }; }
                boolean!(show_images, "showImages"); boolean!(auto_resize_images, "imageAutoResize"); boolean!(block_images, "blockImages"); boolean!(enable_skill_commands, "enableSkillCommands");
                boolean!(hide_thinking_block, "hideThinkingBlock"); boolean!(smooth_streaming, "smoothStreaming"); boolean!(show_cache_miss_notices, "showCacheMissNotices"); boolean!(collapse_changelog, "collapseChangelog");
                boolean!(enable_install_telemetry, "enableInstallTelemetry"); boolean!(show_hardware_cursor, "showHardwareCursor"); boolean!(quiet_startup, "quietStartup"); boolean!(clear_on_shrink, "clearOnShrink"); boolean!(show_terminal_progress, "showTerminalProgress"); boolean!(fullscreen_copy_on_select, "fullscreenCopyOnSelect");
                number!(image_width_cells, "imageWidthCells"); number!(http_idle_timeout_ms, "httpIdleTimeoutMs"); number!(smooth_streaming_fps, "smoothStreamingFps"); number!(editor_padding_x, "editorPaddingX"); number!(output_pad, "outputPad"); number!(autocomplete_max_visible, "autocompleteMaxVisible");
                string!(steering_mode, "steeringMode"); string!(follow_up_mode, "followUpMode"); string!(transport, "transport"); string!(double_escape_action, "doubleEscapeAction"); string!(tree_filter_mode, "treeFilterMode"); string!(fullscreen_scrollbar, "fullscreenScrollbar");
                use crate::components::settings_selector::{MermaidRenderingMode, TuiMode, FullscreenExitOutput, DefaultProjectTrust};
                config.terminal_mouse = settings.get_string("terminalMouse");
                config.mermaid_rendering_mode = settings.get_string("mermaidRenderingMode").and_then(|value| MermaidRenderingMode::from_name(&value)).unwrap_or(config.mermaid_rendering_mode);
                config.tui_mode = settings.get_string("tuiMode").and_then(|value| TuiMode::from_name(&value)).unwrap_or(config.tui_mode);
                config.fullscreen_exit_output = settings.get_string("fullscreenExitOutput").and_then(|value| FullscreenExitOutput::from_name(&value)).unwrap_or(config.fullscreen_exit_output);
                config.default_project_trust = match settings.get_string("defaultProjectTrust").as_deref() { Some("always") => DefaultProjectTrust::Always, Some("never") => DefaultProjectTrust::Never, _ => DefaultProjectTrust::Ask };
                config.warnings.anthropic_extra_usage = settings.get_value("warnings").and_then(|value| value.get("anthropicExtraUsage")).and_then(serde_json::Value::as_bool);
            });
            let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
            let cancelled = self.ui_reply.clone();
            let mut callbacks = SettingsCallbacks { on_cancel:Some(Box::new(move || { cancelled.borrow_mut().take(); })), ..Default::default() };
            macro_rules! persist { ($field:ident, $key:literal, $ty:ty) => {{ let session = self.session.clone(); let ui = self.extension_ui.clone(); callbacks.$field = Some(Box::new(move |value: $ty| {
                let values = [($key.into(), serde_json::json!(value))].into_iter().collect();
                match session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &values)) {
                    Ok(()) => { drop(ui.sender.send(crate::interactive_extension_ui::UiRequest::SettingChanged($key.into(), serde_json::json!(value)))); }
                    Err(error) => maho_ext_api::ExtensionUi::notify(ui.as_ref(), &error, maho_ext_api::NotificationType::Error),
                }
            })); }}; }
            persist!(on_show_images_change, "showImages", bool); persist!(on_image_width_cells_change, "imageWidthCells", usize); persist!(on_auto_resize_images_change, "imageAutoResize", bool); persist!(on_block_images_change, "blockImages", bool); persist!(on_enable_skill_commands_change, "enableSkillCommands", bool);
            persist!(on_transport_change, "transport", String); persist!(on_http_idle_timeout_ms_change, "httpIdleTimeoutMs", u64); persist!(on_theme_change, "theme", String); persist!(on_hide_thinking_block_change, "hideThinkingBlock", bool);
            persist!(on_smooth_streaming_change, "smoothStreaming", bool); persist!(on_smooth_streaming_fps_change, "smoothStreamingFps", u32); persist!(on_show_cache_miss_notices_change, "showCacheMissNotices", bool); persist!(on_collapse_changelog_change, "collapseChangelog", bool); persist!(on_enable_install_telemetry_change, "enableInstallTelemetry", bool);
            persist!(on_double_escape_action_change, "doubleEscapeAction", String); persist!(on_tree_filter_mode_change, "treeFilterMode", String); persist!(on_show_hardware_cursor_change, "showHardwareCursor", bool); persist!(on_editor_padding_x_change, "editorPaddingX", usize); persist!(on_output_pad_change, "outputPad", u8); persist!(on_autocomplete_max_visible_change, "autocompleteMaxVisible", usize);
            persist!(on_quiet_startup_change, "quietStartup", bool); persist!(on_clear_on_shrink_change, "clearOnShrink", bool); persist!(on_show_terminal_progress_change, "showTerminalProgress", bool); persist!(on_terminal_mouse_change, "terminalMouse", String); persist!(on_fullscreen_scrollbar_change, "fullscreenScrollbar", String); persist!(on_fullscreen_copy_on_select_change, "fullscreenCopyOnSelect", bool);
            let session = self.session.clone(); callbacks.on_auto_compact_change = Some(Box::new(move |enabled| { session.set_auto_compaction_enabled(enabled); let values = [("compaction".into(), serde_json::json!({"enabled":enabled}))].into_iter().collect(); if let Err(error) = session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &values)) { session.emit(maho_ext_api::AgentSessionEvent::ContinuationError { error_message:error }); } }));
            let session = self.session.clone(); callbacks.on_steering_mode_change = Some(Box::new(move |mode| session.set_steering_mode(if mode == "all" { maho_agent::types::QueueMode::All } else { maho_agent::types::QueueMode::OneAtATime })));
            let session = self.session.clone(); callbacks.on_follow_up_mode_change = Some(Box::new(move |mode| session.set_follow_up_mode(if mode == "all" { maho_agent::types::QueueMode::All } else { maho_agent::types::QueueMode::OneAtATime })));
            let session = self.session.clone(); callbacks.on_thinking_level_change = Some(Box::new(move |level| session.set_thinking_level(maho_ai::types::ModelThinkingLevel::parse(level.as_str()).expect("selector thinking level"))));
            macro_rules! persist_enum { ($field:ident, $key:literal, $ty:ty, $value:expr) => {{ let session = self.session.clone(); let ui = self.extension_ui.clone(); callbacks.$field = Some(Box::new(move |value: $ty| {
                let serialized = ($value)(value); let values = [($key.into(), serialized.clone())].into_iter().collect();
                match session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &values)) {
                    Ok(()) => { drop(ui.sender.send(crate::interactive_extension_ui::UiRequest::SettingChanged($key.into(), serialized))); }
                    Err(error) => maho_ext_api::ExtensionUi::notify(ui.as_ref(), &error, maho_ext_api::NotificationType::Error),
                }
            })); }}; }
            use crate::components::settings_selector::{MermaidRenderingMode, TuiMode, FullscreenExitOutput, DefaultProjectTrust, WarningSettings};
            persist_enum!(on_mermaid_rendering_mode_change, "mermaidRenderingMode", MermaidRenderingMode, |value: MermaidRenderingMode| serde_json::json!(value.as_str()));
            persist_enum!(on_tui_mode_change, "tuiMode", TuiMode, |value: TuiMode| serde_json::json!(value.as_str()));
            persist_enum!(on_fullscreen_exit_output_change, "fullscreenExitOutput", FullscreenExitOutput, |value: FullscreenExitOutput| serde_json::json!(value.as_str()));
            persist_enum!(on_default_project_trust_change, "defaultProjectTrust", DefaultProjectTrust, |value: DefaultProjectTrust| serde_json::json!(match value { DefaultProjectTrust::Ask => "ask", DefaultProjectTrust::Always => "always", DefaultProjectTrust::Never => "never" }));
            persist_enum!(on_warnings_change, "warnings", WarningSettings, |value: WarningSettings| serde_json::json!({"anthropicExtraUsage":value.anthropic_extra_usage}));
            self.ui_dialog = Some(Box::new(SettingsSelectorComponent::new(&self.theme, config, callbacks, self.session.model().input.contains(&maho_ai::types::InputModality::Image))));
            return Ok(true);
        }
        if text == "/trust" {
            use crate::components::trust_selector::{TrustSelectorComponent, TrustSelectorOptions};
            let cwd = self.session.cwd();
            let store = maho_core::trust_manager::ProjectTrustStore::new(&self.session.agent_dir());
            let saved_decision = store.get_entry(&cwd)?;
            let project_trusted = self.session.with_settings_manager(|settings| settings.is_project_trusted());
            let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
            let selected = self.ui_reply.clone(); let cancelled = selected.clone(); let ui = self.extension_ui.clone();
            self.ui_dialog = Some(Box::new(TrustSelectorComponent::new(&self.theme, Arc::new(self.keybindings()), TrustSelectorOptions {
                cwd, saved_decision, project_trusted,
                on_select:Box::new(move |selection| {
                    match store.set_many(&selection.updates) {
                        Ok(()) => { selected.borrow_mut().take(); maho_ext_api::ExtensionUi::notify(ui.as_ref(), &format!("Saved trust decision: {}. Restart {} for this to take effect.", if selection.trusted { "trusted" } else { "untrusted" }, maho_core::config::app_name()), maho_ext_api::NotificationType::Info); }
                        Err(error) => maho_ext_api::ExtensionUi::notify(ui.as_ref(), &error, maho_ext_api::NotificationType::Error),
                    }
                }), on_cancel:Box::new(move || { cancelled.borrow_mut().take(); }),
            })));
            return Ok(true);
        }
        if matches!(text, "/scoped-models" | "/favorite-models") {
            let favorites = text == "/favorite-models";
            let models = self.session.model_registry().get_available();
            if favorites && models.is_empty() { self.show_status("No models available".into()); return Ok(true); }
            let key = if favorites { "favoriteModels" } else { "enabledModels" };
            let configured: Option<Vec<String>> = self.session.with_settings_manager(|settings| settings.get_value(key).filter(|value| !value.is_null()).cloned()).map(serde_json::from_value).transpose().map_err(|error| error.to_string())?;
            let stored = configured.clone().unwrap_or_default();
            let catalog = self.session.model_registry().get_all();
            let resolution = maho_core::model_resolver::resolve_model_scope_from_models(&stored, &catalog);
            let candidate_ids: Vec<_> = models.iter().map(|model| format!("{}/{}", model.provider, model.id)).collect();
            let entries = if favorites { self.session.favorite_models() } else { self.session.scoped_models() };
            let enabled = if !entries.is_empty() { Some(entries.iter().map(|entry| format!("{}/{}", entry.model.provider, entry.model.id)).collect()) }
                else if stored.is_empty() { if favorites || configured.is_some() { Some(Vec::new()) } else { None } }
                else { Some(resolution.pattern_resolutions.iter().flat_map(|item| if item.unresolved { vec![item.pattern.clone()] } else { item.owned_ids.clone() }).collect()) };
            let session = self.session.clone(); let available = models.clone();
            let on_change = Box::new(move |ids: Option<Vec<String>>| {
                let all_ids: Vec<_> = available.iter().map(|model| format!("{}/{}", model.provider, model.id)).collect();
                let resolved = maho_core::model_resolver::resolve_model_scope_from_models(ids.as_deref().unwrap_or(&all_ids), &available);
                let entries = resolved.scoped_models.into_iter().map(|entry| maho_core::agent_session::SessionModelEntry { model:entry.model, thinking_level:entry.thinking_level.map(|level| serde_json::from_value(serde_json::json!(level.as_str())).expect("resolved thinking level")), thinking_selection:entry.thinking_selection, service_tier:entry.service_tier.map(|tier| match tier.as_str() { "auto" => maho_ext_api::ServiceTier::Auto, "flex" => maho_ext_api::ServiceTier::Flex, "priority" => maho_ext_api::ServiceTier::Priority, _ => unreachable!("resolved service tier") }) }).collect();
                if favorites { session.set_favorite_models(entries); }
                else if ids.as_ref().is_some_and(|ids| available.iter().all(|model| ids.contains(&format!("{}/{}", model.provider, model.id)))) { session.set_scoped_models(Vec::new()); }
                else { session.set_scoped_models(entries); }
            });
            let session = self.session.clone(); let ui = self.extension_ui.clone();
            let on_persist = Box::new(move |ids: Option<Vec<String>>| {
                let patterns = if favorites { crate::components::model_favorites::merge_favorite_patterns_for_persist(crate::components::model_favorites::FavoritePatternsForPersist { stored_patterns:&stored, pattern_resolutions:&resolution.pattern_resolutions, selected_ids:&ids, candidate_ids:&candidate_ids }) }
                    else { ids.filter(|ids| !candidate_ids.iter().all(|id| ids.contains(id))) };
                let values = [(key.into(), serde_json::json!(patterns))].into_iter().collect();
                match session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &values)) {
                    Ok(()) => maho_ext_api::ExtensionUi::notify(ui.as_ref(), if favorites { "Favorite models saved to settings" } else { "Model selection saved to settings" }, maho_ext_api::NotificationType::Info),
                    Err(error) => maho_ext_api::ExtensionUi::notify(ui.as_ref(), &error, maho_ext_api::NotificationType::Error),
                }
            });
            let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
            let cancelled = self.ui_reply.clone();
            let keys = Arc::new(self.keybindings());
            if favorites {
                use crate::components::favorite_models_selector::{FavoriteModelsSelectorComponent, FavoriteModelsConfig, FavoriteModelsCallbacks};
                let selected = self.ui_reply.clone(); let submissions = self.submissions.clone(); let current = self.session.model();
                self.ui_dialog = Some(Box::new(FavoriteModelsSelectorComponent::new(&self.theme, keys,
                    FavoriteModelsConfig { all_models:models.into_iter().map(|model| crate::components::model_selector::ModelEntry { provider:model.provider, id:model.id, name:model.name }).collect(), favorite_model_ids:enabled, current_model:Some(crate::components::model_selector::ModelEntry { provider:current.provider, id:current.id, name:current.name }) },
                    FavoriteModelsCallbacks { on_change, on_persist, on_select:Box::new(move |model| { submissions.borrow_mut().push_back(format!("/model {}/{}", model.provider, model.id)); selected.borrow_mut().take(); }), on_cancel:Box::new(move || { cancelled.borrow_mut().take(); }) })));
            } else {
                use crate::components::scoped_models_selector::{ScopedModelsSelectorComponent, ModelsConfig, ModelsCallbacks};
                self.ui_dialog = Some(Box::new(ScopedModelsSelectorComponent::new(&self.theme, keys,
                    ModelsConfig { all_models:models, enabled_model_ids:enabled, refresh_status:None },
                    ModelsCallbacks { on_change, on_persist, on_cancel:Box::new(move || { cancelled.borrow_mut().take(); }) })));
            }
            return Ok(true);
        }
        if text == "/tree" {
            let (tree, leaf) = self.session.with_session_manager(|manager| {
                let entries = manager.entries();
                let roots = entries.iter().filter(|entry| entry.get("parentId").is_none_or(serde_json::Value::is_null)).filter_map(|entry| entry["id"].as_str()).filter_map(|id| manager.get_tree(Some(id))).collect::<Vec<_>>();
                (roots, manager.leaf_id().map(str::to_owned))
            });
            if tree.is_empty() { self.show_status("No entries in session".into()); return Ok(true); }
            let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
            let selected = self.ui_reply.clone(); let cancelled = selected.clone(); let submissions = self.submissions.clone(); let session = self.session.clone();
            let filter = self.session.with_settings_manager(|settings| settings.get_value("treeFilterMode").and_then(serde_json::Value::as_str).unwrap_or("default").to_owned());
            let filter = match filter.as_str() { "no-tools" => crate::components::tree_selector::FilterMode::NoTools, "user-only" => crate::components::tree_selector::FilterMode::UserOnly, "labeled-only" => crate::components::tree_selector::FilterMode::LabeledOnly, "all" => crate::components::tree_selector::FilterMode::All, _ => crate::components::tree_selector::FilterMode::Default };
            let mut selector = crate::components::tree_selector::TreeSelectorComponent::new(&self.theme,
                Arc::new(self.keybindings()),
                tree, leaf.as_deref(), self.editor_host.terminal_rows(),
                Box::new(move |id| { submissions.borrow_mut().push_back(format!("/tree {id}")); selected.borrow_mut().take(); }), Box::new(move || { cancelled.borrow_mut().take(); }),
                Some(Box::new(move |id, label| session.with_session_manager_mut(|manager| { manager.append_label(id, label); }))), None, Some(filter), std::env::var("HOME").ok());
            let copies=self.tree_copies.clone();
            selector.on_copy=Some(Box::new(move |text|copies.borrow_mut().push_back(text.map(str::to_owned))));
            self.ui_dialog=Some(Box::new(selector));
            return Ok(true);
        }
        if text == "/fork" {
            use crate::components::user_message_selector::{UserMessageItem, UserMessageSelectorComponent};
            let entries = self.session.with_session_manager(|manager| manager.branch(None));
            let messages: Vec<UserMessageItem> = entries.iter().filter(|entry| entry["message"]["role"] == "user").map(|entry| {
                let content = &entry["message"]["content"];
                let text = content.as_str().map(str::to_owned).unwrap_or_else(|| content.as_array().map(|parts| parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default());
                UserMessageItem { id:entry["id"].as_str().expect("entry id").into(), text, timestamp:entry["timestamp"].as_str().map(str::to_owned) }
            }).collect();
            if messages.is_empty() { self.show_status("No messages to fork from".into()); return Ok(true); }
            let initial_selected_id = messages.last().map(|message| message.id.clone());
            let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
            let selected = self.ui_reply.clone(); let cancelled = selected.clone(); let submissions = self.submissions.clone();
            self.ui_dialog = Some(Box::new(UserMessageSelectorComponent::new(messages,
                Box::new(move |id| { submissions.borrow_mut().push_back(format!("/fork {id}")); selected.borrow_mut().take(); }),
                Box::new(move || { cancelled.borrow_mut().take(); }), initial_selected_id.as_deref(), self.theme.clone())));
            if self.ui_reply.borrow().is_none() { self.ui_dialog = None; self.local_dialog_reply = None; }
            return Ok(true);
        }
        if text == "/model" {
            use crate::components::model_selector::{ModelSelectorComponent, ModelEntry, ModelSelectorFavoriteOptions};
            let current = self.session.model();
            let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
            let selected = self.ui_reply.clone(); let cancelled = selected.clone(); let submissions = self.submissions.clone();
            let models = self.session.model_registry().get_available().into_iter().map(|model| ModelEntry { provider:model.provider, id:model.id, name:model.name }).collect::<Vec<_>>();
            let stored: Vec<String> = self.session.with_settings_manager(|settings| settings.get_value("favoriteModels").cloned()).map(serde_json::from_value).transpose().map_err(|error| error.to_string())?.unwrap_or_default();
            let catalog = self.session.model_registry().get_all();
            let resolutions = maho_core::model_resolver::resolve_model_scope_from_models(&stored, &catalog).pattern_resolutions;
            let candidate_ids = models.iter().map(ModelEntry::full_id).collect::<Vec<_>>();
            let session_favorites = self.session.favorite_models().into_iter().map(|entry| format!("{}/{}", entry.model.provider, entry.model.id)).filter(|id| candidate_ids.contains(id)).collect::<Vec<_>>();
            let favorite_ids = Some(if session_favorites.is_empty() { resolutions.iter().flat_map(|resolution| resolution.owned_ids.clone()).filter(|id| candidate_ids.contains(id)).collect() } else { session_favorites });
            let session = self.session.clone(); let ui = self.extension_ui.clone();
            let favorite_callback = Box::new(move |ids: crate::components::favorite_model_ids::FavoriteModelIds, candidates: &[ModelEntry], _: &ModelEntry| {
                let candidate_ids = candidates.iter().map(ModelEntry::full_id).collect::<Vec<_>>();
                let merged = crate::components::model_favorites::merge_favorite_patterns_for_persist(crate::components::model_favorites::FavoritePatternsForPersist { stored_patterns:&stored, pattern_resolutions:&resolutions, selected_ids:&ids, candidate_ids:&candidate_ids });
                let values = [("favoriteModels".into(), serde_json::json!(merged))].into_iter().collect();
                if let Err(error) = session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &values)) { maho_ext_api::ExtensionUi::notify(ui.as_ref(), &error, maho_ext_api::NotificationType::Error); return; }
                let patterns = merged.unwrap_or_default();
                let resolved = maho_core::model_resolver::resolve_model_scope_from_models(&patterns, &catalog);
                session.set_favorite_models(resolved.scoped_models.into_iter().map(|entry| maho_core::agent_session::SessionModelEntry { model:entry.model, thinking_level:entry.thinking_level.and_then(|level| serde_json::from_value(serde_json::json!(level.as_str())).ok()), thinking_selection:entry.thinking_selection, service_tier:entry.service_tier.map(|tier| match tier.as_str() { "auto" => maho_ext_api::ServiceTier::Auto, "flex" => maho_ext_api::ServiceTier::Flex, "priority" => maho_ext_api::ServiceTier::Priority, _ => unreachable!("resolved service tier") }) }).collect());
            });
            let scoped = self.session.scoped_models().into_iter().map(|entry| crate::components::model_selector::ScopedModelItem { model:ModelEntry { provider:entry.model.provider, id:entry.model.id, name:entry.model.name }, thinking_level:entry.thinking_level.map(|level| serde_json::to_value(level).expect("thinking level").as_str().expect("string level").to_owned()) }).collect();
            let mut selector = ModelSelectorComponent::new(&self.theme, Arc::new(self.keybindings()), 0, &models,
                Some(ModelEntry { provider:current.provider, id:current.id, name:current.name }),
                scoped,
                Box::new(move |model| { submissions.borrow_mut().push_back(format!("/model {}/{}", model.provider, model.id)); selected.borrow_mut().take(); }),
                Box::new(move || { cancelled.borrow_mut().take(); }), None,
                ModelSelectorFavoriteOptions { favorite_model_ids:favorite_ids, on_favorite_change:Some(favorite_callback) }, None);
            let session = self.session.clone(); let ui = self.extension_ui.clone();
            selector.set_default_model_change_handler(Box::new(move |model| {
                let values = [("defaultProvider".into(), serde_json::json!(model.provider)), ("defaultModel".into(), serde_json::json!(model.id))].into_iter().collect();
                if let Err(error) = session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &values)) { maho_ext_api::ExtensionUi::notify(ui.as_ref(), &error, maho_ext_api::NotificationType::Error); }
            }));
            self.ui_dialog = Some(Box::new(selector));
            return Ok(true);
        }
        if text == "/thinking" {
            let (reply, _receiver) = tokio::sync::oneshot::channel();
            self.local_dialog_reply = Some(_receiver);
            *self.ui_reply.borrow_mut() = Some(reply);
            let selected = self.ui_reply.clone(); let cancelled = selected.clone(); let session = self.session.clone();
            let default = self.session.with_settings_manager(|settings| settings.get_string("defaultThinkingLevel")).and_then(|value| maho_ai::types::ModelThinkingLevel::parse(&value));
            let default_session = self.session.clone(); let ui = self.extension_ui.clone();
            self.ui_dialog = Some(Box::new(crate::components::thinking_selector::ThinkingSelectorComponent::new(&self.theme,
                Arc::new(self.keybindings()),
                crate::components::thinking_selector::ThinkingSelectorOptions { current:self.session.thinking_level(), available:self.session.get_available_thinking_levels(), default,
                    on_select:Box::new(move |level| { session.set_session_thinking_level(level); selected.borrow_mut().take(); }), on_cancel:Box::new(move || { cancelled.borrow_mut().take(); }), on_select_as_default:Some(Box::new(move |level| {
                        let values = [("defaultThinkingLevel".into(), serde_json::json!(level.as_str()))].into_iter().collect();
                        match default_session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &values)) {
                            Ok(()) => maho_ext_api::ExtensionUi::notify(ui.as_ref(), &format!("Default thinking level: {}", level.as_str()), maho_ext_api::NotificationType::Info),
                            Err(error) => maho_ext_api::ExtensionUi::notify(ui.as_ref(), &error, maho_ext_api::NotificationType::Error),
                        }
                    })) })));
            return Ok(true);
        }
        if matches!(text, "/quit" | "/exit") { self.shutdown_requested = true; return Ok(true); }
        if text == "/session" {
            let stats = self.session.get_session_stats();
            let entries = self.session.with_session_manager(|manager| manager.entries());
            let prices = |provider: &str, model: &str| self.session.model_registry().find(provider, model).map(|model| model.cost.cache_read);
            let waste = maho_core::cache_stats::compute_cache_waste(&entries, &prices).map_err(|error| error.to_string())?;
            let breakdown = maho_core::usage_totals::get_usage_cost_breakdown(&entries);
            let theme = &self.theme;
            let label = |text: &str| theme.fg(crate::theme::ThemeColor::Dim, text);
            let count = crate::components::compaction_summary_message::format_count;
            let mut info = format!("{}\n\n", theme.bold("Session Info"));
            if let Some(name) = self.session.session_name() { info += &format!("{} {name}\n", label("Name:")); }
            info += &format!("{} {}\n{} {}\n\n{}\n{} {}\n{} {}\n{} {}\n{} {} calls, {} results\n\n{}\n", label("File:"), stats.session_file.as_deref().unwrap_or("In-memory"), label("ID:"), stats.session_id, theme.bold("Messages"), label("Total:"), stats.total_messages, label("User:"), stats.user_messages, label("Assistant:"), stats.assistant_messages, label("Tools:"), stats.tool_calls, stats.tool_results, theme.bold("Tokens"));
            let tokens = stats.tokens;
            let prompt = tokens.input + tokens.cache_read + tokens.cache_write;
            info += &format!("{} {}\n", label("Input:"), count(prompt));
            if prompt > 0 && (tokens.cache_read > 0 || tokens.cache_write > 0) {
                info += &format!("  {} {} {}\n  {} {}{}\n", label("Cached:"), count(tokens.cache_read), label(&format!("({:.1}%)", tokens.cache_read as f64 / prompt as f64 * 100.0)), label("Uncached:"), count(tokens.input + tokens.cache_write), if tokens.cache_write > 0 { format!(" {}", label(&format!("({} written to cache)", count(tokens.cache_write)))) } else { String::new() });
            }
            info += &format!("{} {}\n{} {}\n", label("Output:"), count(tokens.output), label("Total:"), count(tokens.total));
            if stats.cost > 0.0 || waste.missed_tokens > 0.0 {
                info += &format!("\n{}\n{} ${:.3}", theme.bold("Cost"), label("Total:"), stats.cost);
                if breakdown.len() > 1 { for entry in breakdown { info += &format!("\n  {} ${:.3} {}", label(&format!("{}:", entry.key)), entry.cost, label(&format!("({} tokens)", crate::components::footer::format_tokens(entry.tokens as f64)))); } }
                if waste.missed_tokens > 0.0 { info += &format!("\n{} {}{}", label("Cache Re-billed:"), if waste.missed_cost >= 0.0001 { format!("${:.3} ", waste.missed_cost) } else { String::new() }, label(&format!("({} tokens, {} miss{})", count(waste.missed_tokens as u64), waste.miss_count, if waste.miss_count == 1 { "" } else { "es" }))); }
            }
            self.show_status(info); return Ok(true);
        }
        if text.starts_with("/export ") {
            let path = get_path_command_argument(text, "/export").ok_or("Missing export path")?;
            if path.ends_with(".jsonl") {
                let exported = self.session.export_to_jsonl(Some(&path)).map_err(|error| format!("Failed to export session: {error}"))?;
                self.show_status(format!("Session exported to: {exported}"));
                return Ok(true);
            }
        }
        if matches!(text, "/rename" | "/name") {
            let accepted = self.rename_result.clone();
            let cancelled = self.rename_result.clone();
            self.rename_input = Some(crate::components::extension_input::ExtensionInputComponent::new(&self.theme, "Rename session", Box::new(move |text| *accepted.borrow_mut() = Some(Some(text.into()))), Box::new(move || *cancelled.borrow_mut() = Some(None)), crate::components::extension_input::ExtensionInputOptions { initial_value: self.session.session_name(), ..Default::default() }));
            return Ok(true);
        }
        if let Some(name) = text.strip_prefix("/rename ").or_else(|| text.strip_prefix("/name ")) {
            let name = name.trim();
            if name.is_empty() { return Err("Session name cannot be empty".into()); }
            self.session.set_session_name(name);
            self.show_status(format!("Session name set: {}", self.session.session_name().unwrap_or_else(|| name.into())));
            return Ok(true);
        }
        if let Some(value) = text.strip_prefix("/thinking ") {
            let level = maho_ai::types::ModelThinkingLevel::parse(value.trim()).ok_or_else(|| format!("Invalid thinking level: {}", value.trim()))?;
            if !self.session.get_available_thinking_levels().contains(&level) { return Err(format!("Thinking level {} is not supported by the current model", value.trim())); }
            self.session.set_session_thinking_level(level);
            self.show_status(format!("Thinking level: {}", level.as_str()));
            return Ok(true);
        }
        if text == "/hotkeys" {
            let markdown = crate::help_content::build_help_markdown(&[]);
            self.chat.add_child(Rc::new(RefCell::new(crate::components::markdown_transform::MarkdownComponent(maho_tui::components::markdown::Markdown::new(&markdown, 1, 1, get_markdown_theme(&self.theme), None, Default::default())))));
            return Ok(true);
        }
        if let Some(name) = text.split_whitespace().next().and_then(|word| word.strip_prefix('/'))
            && maho_core::slash_commands::builtin_slash_commands().iter().any(|command| command.name == name)
        {
            return Err(format!("/{name} is not yet integrated into the native interactive runtime"));
        }
        Ok(false)
    }

    pub fn drain_events(&mut self) {
        self.drain_ui_requests();
        while let Ok(event) = self.events.try_recv() {
            self.handle_session_event(&event);
        }
    }

    pub fn handle_session_event(&mut self, event: &maho_ext_api::AgentSessionEvent) {
        use maho_ext_api::AgentSessionEvent;
        match event {
            AgentSessionEvent::Agent(event) => self.handle_event(event),
            AgentSessionEvent::ContinuationError { error_message } => self.show_status(error_message.clone()),
            AgentSessionEvent::ModelChangePending { notice, .. } | AgentSessionEvent::ResumeCompactionRequired { notice, .. } | AgentSessionEvent::ResumeContextReduced { notice, .. } => self.show_status(notice.clone()),
            AgentSessionEvent::ModelChangeRejected { detail, .. } => self.show_status(detail.clone()),
            AgentSessionEvent::ThinkingLevelChanged { level } => self.show_status(format!("Thinking level: {}", serde_json::to_value(level).expect("level").as_str().expect("string"))),
            AgentSessionEvent::AutoRetryStart { attempt, max_attempts, error_message, .. } => self.show_status(format!("Retrying ({attempt}/{max_attempts}): {error_message}")),
            AgentSessionEvent::AutoRetryEnd { final_error:Some(error), .. } => self.show_status(error.clone()),
            AgentSessionEvent::CompactionStart { .. } => self.show_status("Compacting context...".into()),
            AgentSessionEvent::CompactionEnd { error_message:Some(error), .. } => self.show_status(error.clone()),
            AgentSessionEvent::AgentIdle | AgentSessionEvent::AgentSettled | AgentSessionEvent::SessionAbort => { self.agent_idle = true; self.working_started_ms = None; }
            _ => {}
        }
    }

    fn drain_ui_requests(&mut self) {
        while let Ok(request) = self.ui_requests.try_recv() { self.handle_ui_request(request); }
        if self.question_reply.borrow().is_none() {
            self.question = None; self.async_question_widget = None; self.expanded_question_widget = None;
            while let Some(request) = self.queued_questions.pop_front() {
                self.handle_ui_request(request);
                if self.question_reply.borrow().is_some() { break; }
            }
        }
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let mut index = 0;
        while index < self.custom_ui_builds.len() {
            match self.custom_ui_builds[index].as_mut().poll(&mut context) {
                std::task::Poll::Pending => index += 1,
                std::task::Poll::Ready(result) => {
                    drop(self.custom_ui_builds.remove(index));
                    match result {
                        Ok(mut component) => {
                            if self.custom_ui_result.borrow().is_some() { component.dispose(); }
                            else { self.ui_dialog = Some(component); }
                        }
                        Err(error) => { if let Some(reply) = self.custom_ui_reply.take() { drop(reply.send(Err(error))); } }
                    }
                }
            }
        }
        let closed = self.question_reply.borrow().as_ref().is_some_and(tokio::sync::oneshot::Sender::is_closed);
        if closed { self.question_reply.borrow_mut().take(); self.question = None; self.async_question_widget = None; self.expanded_question_widget = None; }
        let closed = self.ui_reply.borrow().as_ref().is_some_and(tokio::sync::oneshot::Sender::is_closed);
        if closed { self.ui_reply.borrow_mut().take(); if let Some(mut dialog) = self.ui_dialog.take() { dialog.dispose(); } }
        let result = self.custom_ui_result.borrow_mut().take();
        if let Some(result) = result {
            if let Some(handle) = self.custom_overlay.take() { handle.hide(); }
            self.custom_ui_builds.clear();
            if let Some(mut component) = self.ui_dialog.take() { component.dispose(); }
            if let Some(reply) = self.custom_ui_reply.take() { drop(reply.send(Ok(result))); }
        }
    }

    fn handle_ui_request(&mut self, request: crate::interactive_extension_ui::UiRequest) {
        use crate::interactive_extension_ui::UiRequest;
        match request {
            UiRequest::WidgetFactory(key, factory, options) => {
                let host = crate::interactive_ui_host::InteractiveUiHost(self.editor_host.clone(),self.terminal_dimensions.clone());
                let theme = maho_ext_api::ExtensionUi::theme(self.extension_ui.as_ref());
                let component = factory.map(|factory| factory(&host, &theme));
                if let Some(index) = self.widgets.iter().position(|(name,_,_)| name == &key) { let (_,mut previous,_) = self.widgets.remove(index); previous.dispose(); }
                if let Some(component) = component { self.widgets.push((key,component,options.placement)); }
            }
            UiRequest::HeaderFactory(factory) => {
                let host = crate::interactive_ui_host::InteractiveUiHost(self.editor_host.clone(),self.terminal_dimensions.clone());
                self.header = factory.map(|factory| factory(&host, &maho_ext_api::ExtensionUi::theme(self.extension_ui.as_ref())));
            }
            UiRequest::FooterFactory(factory) => {
                let host = crate::interactive_ui_host::InteractiveUiHost(self.editor_host.clone(),self.terminal_dimensions.clone());
                self.footer_data.set_available_provider_count(self.session.model_registry().get_available().iter().map(|model|&model.provider).collect::<std::collections::BTreeSet<_>>().len());
                let data = crate::interactive_ui_host::InteractiveFooterData { provider:self.footer_data.clone(), ui:self.extension_ui.clone() };
                self.footer = factory.map(|factory| factory(&host,&maho_ext_api::ExtensionUi::theme(self.extension_ui.as_ref()),&data));
            }
            UiRequest::CustomFactory(factory, options, reply) => {
                if options.overlay && self.mounted_renderer.is_none() { drop(reply.send(Err(maho_ext_api::ExtensionFailure::new("Custom overlay requires the mounted terminal renderer")))); }
                else {
                    let host = crate::interactive_ui_host::InteractiveUiHost(self.editor_host.clone(),self.terminal_dimensions.clone());
                    let theme = maho_ext_api::ExtensionUi::theme(self.extension_ui.as_ref());
                    let keys = self.keybindings(); let result = self.custom_ui_result.clone();
                    self.custom_ui_reply = Some(reply);
                    let renderer = self.mounted_renderer.as_ref().and_then(std::rc::Weak::upgrade);
                    self.custom_ui_builds.push(Box::pin(async move {
                        let component = factory(&host,&theme,&keys,Rc::new(move |value| { *result.borrow_mut() = Some(value); })).await?;
                        if options.overlay && let Some(renderer) = renderer {
                            let overlay_options = options.overlay_options.map(|options| match options { maho_ext_api::ExtensionOverlayOptions::Static(factory) | maho_ext_api::ExtensionOverlayOptions::Dynamic(factory) => factory() });
                            let component = Rc::new(RefCell::new(crate::interactive_ui_host::BoxedComponent(component)));
                            let handle = renderer.borrow_mut().base_mut().show_overlay(component.clone(),overlay_options);
                            if let Some(callback) = options.on_handle { callback(handle); }
                            Ok(Box::new(crate::interactive_ui_host::OverlayComponent { component, renderer:Rc::downgrade(&renderer) }) as Box<dyn Component>)
                        } else { Ok(component) }
                    }));
                }
            }
            UiRequest::WorkingIndicator(options) => self.working_indicator = options,
            UiRequest::Autocomplete(factory) => {
                let commands = maho_core::slash_commands::builtin_slash_commands().into_iter().map(|command| maho_tui::autocomplete::CommandSpec::command(command.name)).collect();
                let provider = factory(Box::new(maho_tui::autocomplete::CombinedAutocompleteProvider::new(commands, &self.session.cwd(), None)));
                maho_tui::editor_component::EditorComponent::set_autocomplete_provider(&mut self.editor, provider);
            }
            UiRequest::EditorFactory(factory) => {
                let mut replacement = factory.map(|factory| factory(self.editor_host.clone(), editor_theme(&self.theme), &self.keybindings()));
                match (&mut self.custom_editor, &mut replacement) {
                    (Some(source), Some(target)) => { crate::editor_paste_transfer::transfer_editor_content(source.as_ref(), target.as_mut()); }
                    (Some(source), None) => { crate::editor_paste_transfer::transfer_editor_content(source.as_ref(), &mut self.editor); }
                    (None, Some(target)) => { crate::editor_paste_transfer::transfer_editor_content(&self.editor, target.as_mut()); }
                    (None, None) => {}
                }
                if let Some(mut editor) = self.custom_editor.take() { editor.dispose(); }
                self.custom_editor = replacement;
            }
            UiRequest::SettingChanged(key, value) => {
                match key.as_str() {
                    "editorPaddingX" => self.editor.set_padding_x(value.as_u64().expect("editor padding") as usize),
                    "autocompleteMaxVisible" => self.editor.editor.set_autocomplete_max_visible(value.as_u64().expect("autocomplete limit") as usize),
                    "enableSkillCommands" => Self::setup_autocomplete(&self.session, &mut self.editor),
                    "showImages" => { for component in &self.tool_cards { component.borrow_mut().set_show_images(value.as_bool().expect("show images")); } },
                    "imageWidthCells" => { for component in &self.tool_cards { component.borrow_mut().set_image_width_cells(u32::try_from(value.as_u64().expect("image width")).expect("selector image width")); } },
                    "hideThinkingBlock" => { self.reveal.hide_thinking = value.as_bool().expect("thinking visibility"); for component in &self.assistant_cards { component.borrow_mut().set_hide_thinking_block(self.reveal.hide_thinking); } if let Some(component) = &self.streaming { let content = self.reveal.resync_visibility(self.clock.elapsed().as_secs_f64() * 1000.0); component.borrow_mut().update_content(&content, Some(true)); } },
                    "smoothStreaming" => { let smooth = value.as_bool().expect("smooth streaming"); self.reveal.smooth = smooth; self.tool_args_reveal.smooth = smooth; self.tool_reveal.smooth = smooth; self.tick(self.clock.elapsed().as_secs_f64() * 1000.0); },
                    "smoothStreamingFps" => { let fps = value.as_f64().expect("streaming fps"); self.reveal.fps = fps; self.tool_args_reveal.fps = fps; self.tool_reveal.fps = fps; },
                    "theme" => {
                        let setting = value.as_str().expect("theme setting");
                        let terminal_theme = if self.theme.name == "light" { crate::theme::TerminalTheme::Light } else { crate::theme::TerminalTheme::Dark };
                        let name = crate::theme::resolve_theme_setting(Some(setting), terminal_theme).unwrap_or("dark");
                        match crate::theme::registry::ThemeRegistry::new(std::path::Path::new(&self.session.agent_dir()).join("themes"), name, self.theme.get_color_mode()).and_then(|registry| registry.load_theme(name)) {
                            Ok(theme) => { self.theme = theme; self.editor.editor.border_color = editor_theme(&self.theme).border_color; *self.extension_ui.theme.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = maho_ext_api::Theme { name:Some(self.theme.name.clone()), colors:self.theme.resolved_colors(), ..Default::default() }; self.rebuild_history(); },
                            Err(error) => self.show_status(error.to_string()),
                        }
                    }
                    "outputPad" => { for component in &self.assistant_cards { component.borrow_mut().set_output_pad(value.as_u64().expect("output padding") as usize); } if self.agent_idle { self.rebuild_history(); } },
                    "showCacheMissNotices" if self.agent_idle => self.rebuild_history(),
                    _ => self.chat.invalidate(),
                }
            }
            UiRequest::Question { request, options, reply } => {
                if reply.is_closed() { return; }
                if self.question_reply.borrow().is_some() {
                    self.queued_questions.push_back(UiRequest::Question { request, options, reply }); return;
                }
                use crate::components::ask_user_question_state as state;
                let request = state::QuestionRequest { request_id:request.request_id, wait_for_answer:request.wait_for_answer, timeout_ms:request.timeout_ms,
                    questions:request.questions.into_iter().map(|q| state::Question { id:q.id, header:q.header, question:q.question, multi_select:q.multi_select, options:q.options.into_iter().map(|o| state::QuestionOption { label:o.label, description:o.description }).collect() }).collect() };
                *self.question_reply.borrow_mut() = Some(reply); let answer = self.question_reply.clone();
                self.async_question_widget = None;
                if !request.wait_for_answer {
                    let reply = self.question_reply.clone(); let unanswered = request.questions.iter().map(|question| question.id.clone()).collect::<Vec<_>>();
                    self.async_question_widget = Some(crate::components::ask_user_async_widget::AskUserAsyncWidget::new(crate::components::ask_user_async_widget::AskUserAsyncWidgetOptions {
                        request:request.clone(), draft:Default::default(), timeout_ms:options.dialog.timeout_ms.unwrap_or(request.timeout_ms), now_ms:self.clock.elapsed().as_millis() as u64,
                        get_deadline_at_ms:None, pending_count:1, theme:self.theme.clone(), env:std::env::vars().collect(), mouse_capture_active:false,
                        on_option_click:None, on_own_answer_click:None, on_expand_click:None, on_next_question:None,
                        on_expire:Box::new(move || { if let Some(reply) = reply.borrow_mut().take() { drop(reply.send(maho_ext_api::QuestionResponse { status:maho_ext_api::QuestionStatus::TimedOut, answers:Default::default(), comment:None, unanswered:unanswered.clone(), auto_resolved_after_ms:None })); } }),
                    }));
                }
                let mut component_options = crate::components::ask_user_question::AskUserQuestionOptions::new(self.theme.clone());
                component_options.now_ms = self.clock.elapsed().as_millis() as u64;
                component_options.timeout_ms = options.dialog.timeout_ms;
                component_options.on_progress = options.on_progress.map(|callback| Box::new(move |draft: &state::QuestionDraft| callback(maho_ext_api::QuestionDraft { comment:draft.comment.clone(), answers:Some(draft.answers.iter().map(|(id, answer)| (id.clone(), maho_ext_api::QuestionAnswer { selected:answer.selected.clone(), text:answer.text.clone() })).collect()) })) as crate::components::ask_user_question::ProgressCallback);
                self.question = Some(crate::components::ask_user_question::AskUserQuestionComponent::new(request, Box::new(move |response| {
                    let status = match response.status { state::QuestionStatus::Answered => maho_ext_api::QuestionStatus::Answered, state::QuestionStatus::CommentSubmitted => maho_ext_api::QuestionStatus::CommentSubmitted, state::QuestionStatus::Cancelled => maho_ext_api::QuestionStatus::Cancelled, state::QuestionStatus::TimedOut => maho_ext_api::QuestionStatus::TimedOut };
                    let response = maho_ext_api::QuestionResponse { status, comment:response.comment, unanswered:response.unanswered, auto_resolved_after_ms:response.auto_resolved_after_ms,
                        answers:response.answers.into_iter().map(|(id, value)| (id, maho_ext_api::QuestionAnswer { selected:value.selected, text:value.text })).collect() };
                    if let Some(reply) = answer.borrow_mut().take() { drop(reply.send(response)); }
                }), component_options));
            }
            UiRequest::ToolsExpanded(expanded) => self.set_tools_expanded(expanded),
            UiRequest::HiddenThinkingLabel(label) => { self.hidden_thinking_label = label.unwrap_or_else(|| "Thinking...".into()); for card in &self.assistant_cards { card.borrow_mut().set_hidden_thinking_label(&self.hidden_thinking_label); } }
            UiRequest::Editor { title, prefill, reply } => {
                *self.ui_reply.borrow_mut() = Some(reply);
                let selected = self.ui_reply.clone(); let cancelled = selected.clone();
                self.ui_dialog = Some(Box::new(crate::components::extension_editor::ExtensionEditorComponent::new(&self.theme, self.editor_host.clone(),
                    Arc::new(self.keybindings()), &title, prefill.as_deref(),
                    Box::new(move |text| { if let Some(reply) = selected.borrow_mut().take() { drop(reply.send(Some(text.into()))); } }),
                    Box::new(move || { if let Some(reply) = cancelled.borrow_mut().take() { drop(reply.send(None)); } }), Default::default(), None, None)));
            }
            UiRequest::WorkingMessage(message) => self.working_message = message,
            UiRequest::WorkingVisible(visible) => self.working_visible = visible,
            UiRequest::Notify(text, _) => self.show_status(text),
            UiRequest::Title(title) => self.terminal_title = Some(title),
            UiRequest::EditorText(text) => self.editor.editor.set_text(&text),
            UiRequest::Paste(text) => self.editor.editor.insert_text_at_cursor(&text),
            UiRequest::Header(factory) => self.header = factory.map(|factory| factory(&maho_ext_api::ExtensionUi::theme(self.extension_ui.as_ref()))),
            UiRequest::Footer(factory) => self.footer = factory.map(|factory| factory(&maho_ext_api::ExtensionUi::theme(self.extension_ui.as_ref()))),
            UiRequest::Widget(key, content, options) => {
                let index = self.widgets.iter().position(|(name, _, _)| name == &key);
                if let Some(content) = content {
                    let component: Box<dyn Component> = match content {
                        maho_ext_api::WidgetContent::Lines(lines) => {
                            let mut container = Container::new();
                            for line in lines.iter().take(10) { container.add_child(Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding(line.clone(), 1, 0)))); }
                            if lines.len() > 10 { container.add_child(Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding(self.theme.fg(crate::theme::ThemeColor::Muted, "... (widget truncated)"), 1, 0)))); }
                            Box::new(container)
                        }
                        maho_ext_api::WidgetContent::Component(factory) => factory(&maho_ext_api::ExtensionUi::theme(self.extension_ui.as_ref())),
                    };
                    if let Some(index) = index { self.widgets[index].1.dispose(); self.widgets[index] = (key, component, options.placement); }
                    else { self.widgets.push((key, component, options.placement)); }
                } else if let Some(index) = index { let (_, mut component, _) = self.widgets.remove(index); component.dispose(); }
            }
            UiRequest::Select { title, options, reply } => {
                *self.ui_reply.borrow_mut() = Some(reply);
                let selected = self.ui_reply.clone(); let cancelled = self.ui_reply.clone();
                let keys = Arc::new(self.keybindings());
                self.ui_dialog = Some(Box::new(crate::components::extension_selector::ExtensionSelectorComponent::new(&self.theme, keys, &title, options,
                    Box::new(move |text| { if let Some(reply) = selected.borrow_mut().take() { drop(reply.send(Some(text.into()))); } }),
                    Box::new(move || { if let Some(reply) = cancelled.borrow_mut().take() { drop(reply.send(None)); } }), Default::default())));
            }
            UiRequest::Input { title, reply } => {
                *self.ui_reply.borrow_mut() = Some(reply);
                let selected = self.ui_reply.clone(); let cancelled = self.ui_reply.clone();
                self.ui_dialog = Some(Box::new(crate::components::extension_input::ExtensionInputComponent::new(&self.theme, &title,
                    Box::new(move |text| { if let Some(reply) = selected.borrow_mut().take() { drop(reply.send(Some(text.into()))); } }),
                    Box::new(move || { if let Some(reply) = cancelled.borrow_mut().take() { drop(reply.send(None)); } }), Default::default())));
            }
        }
        *self.extension_ui.editor_text.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = self.editor.editor.get_text();
    }

    pub fn footer_snapshot(&self) -> crate::components::footer::FooterSnapshot {
        let stats = self.session.get_session_stats();
        let model = self.session.model();
        let usage = self.session.get_context_usage();
        crate::components::footer::FooterSnapshot {
            cwd: self.session.cwd(), home: std::env::var("HOME").ok(), session_name: self.session.session_name(),
            cache_read: stats.tokens.cache_read as f64, cache_write: stats.tokens.cache_write as f64, cost: stats.cost,
            context_window: model.context_window as f64, context_percent: usage.and_then(|usage| usage.percent),
            context_tokens: usage.and_then(|usage| usage.tokens).map(|tokens| tokens as f64),
            model_id: Some(model.id), provider: Some(model.provider), reasoning: model.reasoning,
            thinking_level: Some(self.session.thinking_level().as_str().into()), ..Default::default()
        }
    }

    pub fn handle_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::AgentStart => { self.agent_idle = false; self.pending_tools.clear(); self.tool_partial_json.clear(); self.tool_args_reveal.stop(); self.working_started_ms = Some(self.clock.elapsed().as_secs_f64() * 1000.0); }
            AgentEvent::AgentEnd { .. } => { self.agent_idle = true; self.pending_tools.clear(); self.tool_partial_json.clear(); self.tool_args_reveal.stop(); self.tool_reveal.stop(); self.working_started_ms = None; }
            AgentEvent::MessageStart { message } => {
                if message.role() == "user" {
                    let value = serde_json::to_value(message).expect("serializable agent message");
                    let text = value["content"].as_str().map(str::to_owned).unwrap_or_else(|| value["content"].as_array().map(|parts| parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default());
                    if let Some(block) = maho_core::skill_invocation::parse_skill_block(&text) {
                        let mut component = crate::components::skill_invocation_message::SkillInvocationMessageComponent::new(block.skills.into_iter().map(|skill| crate::components::skill_invocation_message::InvokedSkill { name:skill.name, content:skill.content }).collect(), self.theme.clone(), get_markdown_theme(&self.theme), crate::components::keybinding_hints::key_display_text("app.tools.expand"));
                        component.set_expanded(self.tools_expanded);
                        let component = Rc::new(RefCell::new(component)); self.chat.add_child(component.clone());
                        self.history_expansion.push(Box::new(move |expanded| component.borrow_mut().set_expanded(expanded)));
                        if let Some(text) = block.user_message { self.chat.add_child(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1)))); self.chat.add_child(Rc::new(RefCell::new(UserMessageComponent::new(text, self.theme.clone(), get_markdown_theme(&self.theme), self.output_pad(), self.markdown_transformers.clone())))); }
                        return;
                    }
                    let component = Rc::new(RefCell::new(UserMessageComponent::new(text.clone(), self.theme.clone(), get_markdown_theme(&self.theme), self.output_pad(), self.markdown_transformers.clone())));
                    if let Some(frame) = crate::components::ask_user_answer_chip::parse_ask_user_answer_frame(&text) {
                        let entries = self.session.with_session_manager(|manager| manager.entries());
                        let headers = crate::components::ask_user_answer_chip::get_ask_user_answer_headers(&entries, &frame.request_id);
                        self.chat.add_child(Rc::new(RefCell::new(crate::components::ask_user_answer_chip::AskUserAnswerChip::new(&frame, &headers, component, self.theme.clone()))));
                    } else { self.chat.add_child(component); }
                } else if message.role() == "assistant" {
                    self.assistant_segments.clear();
                    self.reveal.begin(serde_json::to_value(message).expect("assistant"), self.clock.elapsed().as_secs_f64() * 1000.0);
                    let component = Rc::new(RefCell::new(AssistantMessageComponent::new(None, false, get_markdown_theme(&self.theme), "Thinking…", self.output_pad(), self.markdown_transformers.clone(), self.theme.clone())));
                    self.chat.add_child(component.clone());
                    self.assistant_cards.push(component.clone());
                    component.borrow_mut().set_hidden_thinking_label(&self.hidden_thinking_label);
                    self.streaming = Some(component);
                } else if message.role() == "custom" { self.add_history_message(message); }
            }
            AgentEvent::MessageUpdate { message, .. } | AgentEvent::MessageEnd { message } if message.role() == "assistant" => {
                if let Some(assistant) = message.as_assistant() {
                    let final_message = matches!(event, AgentEvent::MessageEnd { .. });
                    if let AgentEvent::MessageUpdate { assistant_message_event: maho_ai::types::AssistantMessageEvent::ToolcallDelta { content_index, delta, .. }, .. } = event
                        && let Some(maho_ai::types::ContentBlock::ToolCall(call)) = assistant.content.get(*content_index) {
                        self.tool_partial_json.entry(call.id.clone()).or_default().push_str(delta);
                    }
                    let mut start = 0;
                    for (index, content) in assistant.content.iter().enumerate() {
                        if let maho_ai::types::ContentBlock::ToolCall(call) = content {
                            self.update_assistant_segment(assistant, start, index, final_message);
                            let args = serde_json::Value::Object(call.arguments.clone());
                            let component = self.tool_component(&call.name, &call.id, args.clone());
                            if final_message { self.tool_args_reveal.finish(&call.id); self.tool_partial_json.remove(&call.id); component.borrow_mut().update_args(args); }
                            else if let Some(partial) = self.tool_partial_json.get(&call.id) {
                                let (handled, value) = self.tool_args_reveal.update(&call.id, Rc::as_ptr(&component) as usize, partial, self.clock.elapsed().as_secs_f64() * 1000.0);
                                if let Some(value) = value { component.borrow_mut().update_args(value); }
                                else if !handled { component.borrow_mut().update_args(args); }
                            } else { component.borrow_mut().update_args(args); }
                            if matches!(event, AgentEvent::MessageEnd { .. }) { component.borrow_mut().set_args_complete(); }
                            start = index + 1;
                        }
                    }
                    self.update_assistant_segment(assistant, start, assistant.content.len(), final_message);
                    if matches!(event, AgentEvent::MessageEnd { .. }) && matches!(assistant.stop_reason, maho_ai::types::StopReason::Aborted | maho_ai::types::StopReason::Error) {
                        for component in self.pending_tools.values() {
                            component.borrow_mut().update_result(ToolExecutionResult { content: vec![maho_tools::definition::ToolContent::text(assistant.error_message.as_deref().unwrap_or("Error"))], details: None, is_error: true }, false);
                        }
                        self.pending_tools.clear();
                    }
                }
                if matches!(event, AgentEvent::MessageEnd { .. }) { self.reveal.stop(); self.streaming = None; self.assistant_segments.clear(); }
            }
            AgentEvent::ToolExecutionStart { tool_call_id, tool_name, args } => {
                self.tool_args_reveal.finish(tool_call_id); self.tool_partial_json.remove(tool_call_id);
                let component = self.tool_component(tool_name, tool_call_id, args.clone());
                let mut component = component.borrow_mut();
                component.update_args(args.clone());
                component.mark_execution_started();
            }
            AgentEvent::ToolExecutionUpdate { tool_call_id, partial_result, .. } => {
                if let Some(component) = self.pending_tools.get(tool_call_id) {
                    let (handled, value) = self.tool_reveal.update(tool_call_id, Rc::as_ptr(component) as usize, partial_result.clone(), self.clock.elapsed().as_secs_f64() * 1000.0);
                    if let Some(value) = value { component.borrow_mut().update_result(Self::tool_result(&value, false), true); }
                    else if !handled { component.borrow_mut().update_result(Self::tool_result(partial_result, false), true); }
                }
            }
            AgentEvent::ToolExecutionEnd { tool_call_id, tool_name, result, is_error } => {
                self.tool_reveal.finish(tool_call_id, self.clock.elapsed().as_secs_f64() * 1000.0);
                let component = self.tool_component(tool_name, tool_call_id, serde_json::json!({}));
                component.borrow_mut().update_result(Self::tool_result(result, *is_error), false);
                self.pending_tools.remove(tool_call_id);
            }
            _ => {}
        }
    }

    fn update_assistant_segment(&mut self, message: &maho_ai::types::AssistantMessage, start: usize, end: usize, final_message: bool) {
        if start == end && start != 0 { return; }
        let output_pad = self.output_pad();
        let component = if start == 0 { self.streaming.clone() } else {
            Some(self.assistant_segments.entry(start).or_insert_with(|| {
                let component = Rc::new(RefCell::new(AssistantMessageComponent::new(None, false, get_markdown_theme(&self.theme), "Thinking…", output_pad, self.markdown_transformers.clone(), self.theme.clone())));
                component.borrow_mut().set_hidden_thinking_label(&self.hidden_thinking_label);
                self.chat.add_child(component.clone()); self.assistant_cards.push(component.clone()); component
            }).clone())
        };
        if let Some(component) = component {
            let mut segment = message.clone(); segment.content = message.content[start..end].to_vec();
            if end < message.content.len() { segment.error_message = None; segment.stop_reason = maho_ai::types::StopReason::ToolUse; }
            let value = serde_json::to_value(segment).expect("assistant segment");
            let value = if start == 0 && !final_message { self.reveal.set_target(value, self.clock.elapsed().as_secs_f64() * 1000.0) } else { value };
            component.borrow_mut().update_content(&value, Some(!final_message));
            component.borrow_mut().set_hide_thinking_block(self.reveal.hide_thinking);
            component.borrow_mut().set_expanded(self.tools_expanded);
        }
    }

    pub fn tick(&mut self, now_ms: f64) {
        self.footer_data.set_cwd(&self.session.cwd());
        self.footer_data.set_available_provider_count(self.session.model_registry().get_available().iter().map(|model|&model.provider).collect::<std::collections::BTreeSet<_>>().len());
        self.footer_data.refresh_branch();
        for (id, value) in self.tool_args_reveal.tick(now_ms) { if let Some(component) = self.pending_tools.get(&id) { component.borrow_mut().update_args(value); } }
        if let Some(widget) = &mut self.async_question_widget { widget.tick(now_ms.max(0.0) as u64); }
        else if let Some(question) = &mut self.question { question.tick(now_ms.max(0.0) as u64); }
        if let Some(widget) = &mut self.expanded_question_widget { widget.tick(now_ms.max(0.0) as u64); }
        if self.question_reply.borrow().is_none() { self.question = None; self.async_question_widget = None; self.expanded_question_widget = None; }
        for component in &self.tool_cards { let mut component = component.borrow_mut(); component.set_now_ms(now_ms); component.tick(now_ms.max(0.0) as u64); }
        if let Some(value) = self.reveal.tick(now_ms) && let Some(component) = &self.streaming { component.borrow_mut().update_content(&value, Some(true)); }
        for (id, value) in self.tool_reveal.tick(now_ms) { if let Some(component) = self.pending_tools.get(&id) { component.borrow_mut().update_result(Self::tool_result(&value, false), true); } }
    }

    pub fn working_frame(&self, now_ms: f64) -> Option<String> {
        if !self.working_visible { return None; }
        let elapsed = (now_ms - self.working_started_ms?).max(0.0);
        if let Some(options) = &self.working_indicator && let Some(frames) = &options.frames && !frames.is_empty() {
            let index = (elapsed / options.interval_ms.unwrap_or(80).max(1) as f64) as usize % frames.len();
            return Some(frames[index].clone());
        }
        let base = |text: &str| self.theme.fg(crate::theme::ThemeColor::Dim, text);
        let glow = |text: &str| self.theme.fg(crate::theme::ThemeColor::Text, text);
        let highlight = |text: &str| self.theme.bold(&glow(text));
        Some(crate::working_status::format_working_status_message_frame(self.working_message.as_deref().unwrap_or("Working"), elapsed / 1000.0,
            &crate::components::keybinding_hints::key_display_text("app.interrupt"), elapsed,
            &crate::working_status::WorkingStatusTextFrameStyle { base:&base, glow:&glow, highlight:&highlight, shimmer:None }, &base))
    }

    fn tool_component(&mut self, name: &str, id: &str, args: serde_json::Value) -> Rc<RefCell<ToolExecutionComponent>> {
        let name = self.session.resolve_tool_call_name(name);
        let options = self.session.with_settings_manager(|settings| ToolExecutionOptions { show_images:settings.get_bool("showImages"), image_width_cells:settings.get_number("imageWidthCells").map(|width| width as u32) });
        self.pending_tools.entry(id.into()).or_insert_with(|| {
            let component = Rc::new(RefCell::new(ToolExecutionComponent::new(&name, id, args, options, None, &self.session.cwd(), ToolExecutionPresentation::Classic, None, self.theme.clone())));
            self.chat.add_child(component.clone());
            component.borrow_mut().set_expanded(self.tools_expanded);
            self.tool_cards.push(component.clone());
            component
        }).clone()
    }

    fn tool_result(value: &serde_json::Value, is_error: bool) -> ToolExecutionResult {
        ToolExecutionResult { content: serde_json::from_value(value["content"].clone()).expect("typed tool result content"), details: value.get("details").cloned(), is_error }
    }
}

impl crate::replay_assistant_tools::ReplayToolHost for InteractiveMode {
    fn expanded(&self) -> bool { self.tools_expanded }
    fn add_message(&mut self, message: maho_ai::types::AssistantMessage) {
        let component = Rc::new(RefCell::new(AssistantMessageComponent::new(Some(serde_json::to_value(message).expect("assistant")), self.reveal.hide_thinking, get_markdown_theme(&self.theme), "Thinking…", self.output_pad(), self.markdown_transformers.clone(), self.theme.clone())));
        component.borrow_mut().set_expanded(self.tools_expanded);
        component.borrow_mut().set_hidden_thinking_label(&self.hidden_thinking_label);
        self.chat.add_child(component.clone()); self.assistant_cards.push(component);
    }
    fn add_child(&mut self, component: Rc<RefCell<ToolExecutionComponent>>) { self.chat.add_child(component.clone()); self.tool_cards.push(component); }
    fn create_tool(&mut self, name: &str, id: &str, args: &serde_json::Map<String, serde_json::Value>) -> ToolExecutionComponent {
        let name = self.session.resolve_tool_call_name(name);
        let options = self.session.with_settings_manager(|settings| ToolExecutionOptions { show_images:settings.get_bool("showImages"), image_width_cells:settings.get_number("imageWidthCells").map(|width| width as u32) });
        ToolExecutionComponent::new(&name, id, serde_json::Value::Object(args.clone()), options, None, &self.session.cwd(), ToolExecutionPresentation::Classic, None, self.theme.clone())
    }
    fn add_pending(&mut self, id: &str, component: Rc<RefCell<ToolExecutionComponent>>) { self.pending_tools.insert(id.into(), component); }
}

pub fn get_path_command_argument(text: &str, command: &str) -> Option<String> {
    let args = text.strip_prefix(command)?.strip_prefix(' ')?.trim_start();
    let first = args.chars().next()?;
    let path = if matches!(first, '\'' | '"') { let end = args[1..].find(first)? + 1; &args[1..end] }
        else { args.split_whitespace().next()? };
    if path == "~" { std::env::var("HOME").ok() }
    else if let Some(relative) = path.strip_prefix("~/") { std::env::var("HOME").ok().map(|home| format!("{home}/{relative}")) }
    else { Some(path.into()) }
}

impl Component for InteractiveMode {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.drain_events();
        self.tick(self.clock.elapsed().as_secs_f64() * 1000.0);
        let mut lines=self.render_document(width);
        lines.extend(self.render_dock(width));
        lines
    }
    fn handle_input(&mut self, data: &str) {
        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system clock").as_millis();
        self.handle_input_at(data, u64::try_from(now_ms).expect("timestamp"));
    }
    fn has_input_handler(&self) -> bool { true }
    fn focusable_get(&self) -> Option<bool> { Some(maho_tui::tui::Focusable::focused(&self.editor)) }
    fn focusable_set(&mut self, focused: bool) { maho_tui::tui::Focusable::set_focused(&mut self.editor, focused); }
    fn invalidate(&mut self) { self.chat.invalidate(); self.editor.invalidate(); }
}

impl InteractiveMode {
    pub(crate) fn render_document(&mut self,width:usize)->Vec<String> {
        let mut lines = self.chat.render(width);
        if let Some(header) = &mut self.header { let mut top = header.render(width); top.extend(lines); lines = top; }
        lines
    }
    pub(crate) fn render_dock(&mut self,width:usize)->Vec<String> {
        let mut lines=Vec::new();
        if let Some(frame) = self.working_frame(self.clock.elapsed().as_secs_f64() * 1000.0) { lines.extend(maho_tui::components::text::Text::with_padding(frame, 1, 0).render(width)); }
        lines.push(String::new());
        for (_, widget, placement) in &mut self.widgets { if *placement == maho_ext_api::WidgetPlacement::AboveEditor { lines.extend(widget.render(width)); } }
        if self.shortcut_overlay { lines.extend(crate::components::shortcut_overlay::ShortcutOverlay::new(&self.theme).render(width)); }
        if let Some(widget) = &mut self.async_question_widget { lines.extend(widget.render(width)); }
        lines.extend(if self.async_question_widget.is_none() && let Some(question) = &mut self.question { question.render(width) } else if let Some(dialog) = &mut self.ui_dialog { dialog.render(width) } else if let Some(input) = &mut self.rename_input { input.render(width) } else if let Some(editor) = &mut self.custom_editor { editor.render(width) } else { self.editor.render(width) });
        for (_, widget, placement) in &mut self.widgets { if *placement == maho_ext_api::WidgetPlacement::BelowEditor { lines.extend(widget.render(width)); } }
        let mut footer = crate::components::footer::FooterComponent::new(self.footer_snapshot());
        footer.set_auto_compact_enabled(self.session.auto_compaction_enabled());
        footer.snapshot.extension_statuses = self.extension_ui.statuses.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        lines.extend(if let Some(custom) = &mut self.footer { custom.render(width) } else { footer.render(width, &self.theme).expect("footer layout") });
        lines
    }
}

impl InteractiveMode {
    fn handle_editor_input(&mut self, data: &str) {
        let submit = self.keybindings().matches(data, "tui.input.submit");
        if let Some(dialog) = &mut self.ui_dialog {
            dialog.handle_input(data);
            if self.ui_reply.borrow().is_none() && self.custom_ui_reply.is_none() { self.ui_dialog = None; self.local_dialog_reply = None; }
            return;
        }
        if let Some(input) = &mut self.rename_input {
            input.handle_input(data);
            let result = self.rename_result.borrow_mut().take();
            if let Some(result) = result {
                self.rename_input = None;
                if let Some(name) = result {
                    let name = name.trim();
                    if name.is_empty() { self.show_status("Session name cannot be empty".into()); }
                    else { self.session.set_session_name(name); self.show_status(format!("Session name set: {}", self.session.session_name().unwrap_or_else(|| name.into()))); }
                }
            }
        } else if let Some(editor) = &mut self.custom_editor {
            if submit {
                let text = editor.get_expanded_text().unwrap_or_else(|| editor.get_text());
                if !text.trim().is_empty() { self.submissions.borrow_mut().push_back(text); editor.set_text(""); }
            } else { editor.handle_input(data); }
        } else { self.editor.handle_input(data); }
        let text = self.custom_editor.as_ref().map_or_else(|| self.editor.editor.get_text(), |editor| editor.get_text());
        *self.extension_ui.editor_text.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = text;
    }
}
