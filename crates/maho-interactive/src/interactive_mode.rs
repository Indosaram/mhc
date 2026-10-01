//! Native user/assistant transcript slice of interactive-mode.ts.
//! Command dispatch, runtime replacement and extension UI remain partial.
use std::{cell::RefCell, collections::BTreeMap, rc::Rc, sync::Arc};
use maho_agent::types::AgentEvent;
use maho_core::agent_session::{AgentSession, AgentSessionSubscription, PromptDisposition, PromptOptions};
use maho_tui::tui::{Component, Container};
use crate::{components::{assistant_message::AssistantMessageComponent, user_message::UserMessageComponent, markdown_transform::get_markdown_theme}, theme::Theme};
use crate::components::{tool_execution::{ToolExecutionComponent, ToolExecutionOptions, ToolExecutionPresentation}, tool_execution_types::ToolExecutionResult};
use crate::components::{custom_editor::{CustomEditor, CustomEditorOptions}, extension_editor::editor_theme};

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
    widgets: Vec<(String, Box<dyn Component>, maho_ext_api::WidgetPlacement)>,
    pub terminal_title: Option<String>,
    markdown_transformers: Vec<crate::components::markdown_transform::MarkdownTransformer>,
    reveal: crate::streaming_reveal::StreamingRevealController,
    clock: std::time::Instant,
    tool_reveal: crate::tool_result_reveal::ToolResultRevealController,
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
}

impl InteractiveMode {
    pub fn new(session: Arc<AgentSession>, theme: Theme, host: Rc<dyn maho_tui::components::editor::EditorTuiHost>) -> Self {
        let (sender, events) = tokio::sync::mpsc::unbounded_channel();
        let subscription = session.subscribe(Arc::new(move |event| { drop(sender.send(event.clone())); }));
        let submissions = Rc::new(RefCell::new(std::collections::VecDeque::new()));
        let captured = submissions.clone();
        let keys = Arc::new(maho_tui::keybindings::KeybindingsManager::new(maho_core::keybindings::keybindings().clone(), Default::default()));
        let mut editor = CustomEditor::new(host.clone(), editor_theme(&theme), keys, CustomEditorOptions::default());
        editor.editor.on_submit = Some(Box::new(move |text| { if !text.trim().is_empty() { captured.borrow_mut().push_back(text.trim().into()); } }));
        let (extension_ui, ui_requests) = crate::interactive_extension_ui::InteractiveExtensionUi::channel(maho_ext_api::Theme { name: Some(theme.name.clone()), colors: theme.resolved_colors(), ..Default::default() });
        let (smooth, fps, hide) = session.with_settings_manager(|settings| (settings.get_bool("smoothStreaming").unwrap_or(true), settings.get_number("smoothStreamingFps").unwrap_or(60.0), settings.get_bool("hideThinkingBlock").unwrap_or(false)));
        Self { session, events, _subscription: subscription, chat: Container::new(), streaming: None, assistant_segments: BTreeMap::new(), pending_tools: BTreeMap::new(), theme, editor, submissions, rename_input: None, rename_result: Rc::new(RefCell::new(None)), shortcut_overlay: false, last_clear_ms: None, shutdown_requested: false, agent_idle: true, extension_ui, ui_requests, ui_dialog: None, ui_reply: Rc::new(RefCell::new(None)), header: None, footer: None, widgets: Vec::new(), terminal_title: None, markdown_transformers: Vec::new(), reveal: crate::streaming_reveal::StreamingRevealController::new(smooth, fps, hide), clock: std::time::Instant::now(), tool_reveal: crate::tool_result_reveal::ToolResultRevealController::new(smooth, fps), last_status: None, assistant_cards: Vec::new(), tool_cards: Vec::new(), tools_expanded: false, local_dialog_reply: None, working_started_ms: None, working_message: None, working_visible: true, editor_host: host, hidden_thinking_label:"Thinking...".into() }
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

    pub fn rebuild_history(&mut self) {
        self.chat.clear(); self.pending_tools.clear(); self.streaming = None; self.assistant_segments.clear();
        self.assistant_cards.clear(); self.tool_cards.clear(); self.last_status = None;
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
                if let Some(parts) = value["content"].as_array() { let text = parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n"); self.editor.editor.add_to_history(&text); }
            }
            AgentMessage::Custom(CustomAgentMessage::Custom(message)) if message.display => {
                self.chat.add_child(Rc::new(RefCell::new(crate::components::custom_message::CustomMessageComponent::new(serde_json::to_value(message).expect("custom message"), None, self.theme.clone(), get_markdown_theme(&self.theme), 1))));
            }
            AgentMessage::Custom(CustomAgentMessage::BranchSummary(message)) => {
                self.chat.add_child(Rc::new(RefCell::new(crate::components::branch_summary_message::BranchSummaryMessageComponent::new(message.summary.clone(), self.theme.clone(), get_markdown_theme(&self.theme), crate::components::keybinding_hints::key_display_text("app.tools.expand")))));
            }
            AgentMessage::Custom(CustomAgentMessage::CompactionSummary(message)) => {
                self.chat.add_child(Rc::new(RefCell::new(crate::components::compaction_summary_message::CompactionSummaryMessageComponent::new(serde_json::to_value(message).expect("summary"), self.theme.clone(), get_markdown_theme(&self.theme), crate::components::keybinding_hints::key_display_text("app.tools.expand")))));
            }
            AgentMessage::Custom(CustomAgentMessage::BashExecution(message)) => {
                let mut component = crate::components::bash_execution::BashExecutionComponent::new(&message.command, message.exclude_from_context.unwrap_or(false), self.theme.clone());
                component.append_output(&message.output);
                component.set_complete(message.exit_code.map(|code| i32::try_from(code).expect("exit code")), message.cancelled, None, message.full_output_path.clone());
                self.chat.add_child(Rc::new(RefCell::new(component)));
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
        let listeners = self.extension_ui.terminal_input.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let mut data = data.to_owned();
        for listener in listeners {
            if let Some(result) = listener(&data) {
                if result.consume.unwrap_or(false) { return; }
                if let Some(replacement) = result.data { data = replacement; }
            }
        }
        self.handle_filtered_input_at(&data, now_ms);
    }

    fn handle_filtered_input_at(&mut self, data: &str, now_ms: u64) {
        if self.ui_dialog.is_some() || self.rename_input.is_some() { self.handle_editor_input(data); return; }
        if self.shortcut_overlay { self.shortcut_overlay = false; return; }
        let keys = maho_tui::keybindings::KeybindingsManager::new(maho_core::keybindings::keybindings().clone(), Default::default());
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
            if let Err(error) = self.session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &[("hideThinkingBlock".into(), serde_json::json!(hidden))].into_iter().collect())) { self.show_status(error); }
            for component in &self.assistant_cards { component.borrow_mut().set_hide_thinking_block(self.reveal.hide_thinking); }
            self.show_status(format!("Thinking blocks: {}", if hidden { "hidden" } else { "visible" }));
            return;
        }
        if data == "?" && self.editor.editor.get_text().is_empty() { self.shortcut_overlay = true; return; }
        self.handle_editor_input(data);
    }

    pub async fn submit_editor(&mut self) -> Result<Option<PromptDisposition>, String> {
        let text = self.submissions.borrow_mut().pop_front();
        let Some(text) = text else { return Ok(None); };
        self.editor.editor.add_to_history(&text);
        let options = PromptOptions { streaming_behavior: Some(maho_ext_api::StreamingBehavior::Steer), ..Default::default() };
        self.submit(&text, options).await.map(Some)
    }

    pub fn set_tools_expanded(&mut self, expanded: bool) {
        self.tools_expanded = expanded;
        for component in &self.tool_cards { component.borrow_mut().set_expanded(expanded); }
        for component in &self.assistant_cards { component.borrow_mut().set_expanded(expanded); }
        self.show_status(format!("Tool output: {}", if expanded { "expanded" } else { "collapsed" }));
    }

    pub async fn submit(&mut self, text: &str, options: PromptOptions) -> Result<PromptDisposition, String> {
        if self.dispatch_command(text)? { return Ok(PromptDisposition::Handled); }
        let session = self.session.clone();
        let prompt = session.prompt(text, options);
        tokio::pin!(prompt);
        let result = loop {
            tokio::select! {
                result = &mut prompt => break result,
                Some(event) = self.events.recv() => {
                    if let maho_ext_api::AgentSessionEvent::Agent(event) = event { self.handle_event(&event); }
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
        if text == "/thinking" {
            let (reply, _receiver) = tokio::sync::oneshot::channel();
            self.local_dialog_reply = Some(_receiver);
            *self.ui_reply.borrow_mut() = Some(reply);
            let selected = self.ui_reply.clone(); let cancelled = selected.clone(); let session = self.session.clone();
            self.ui_dialog = Some(Box::new(crate::components::thinking_selector::ThinkingSelectorComponent::new(&self.theme,
                Arc::new(maho_tui::keybindings::KeybindingsManager::new(maho_core::keybindings::keybindings().clone(), Default::default())),
                crate::components::thinking_selector::ThinkingSelectorOptions { current:self.session.thinking_level(), available:self.session.get_available_thinking_levels(), default:None,
                    on_select:Box::new(move |level| { session.set_session_thinking_level(level); selected.borrow_mut().take(); }), on_cancel:Box::new(move || { cancelled.borrow_mut().take(); }), on_select_as_default:None })));
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
        Ok(false)
    }

    pub fn drain_events(&mut self) {
        self.drain_ui_requests();
        while let Ok(event) = self.events.try_recv() {
            if let maho_ext_api::AgentSessionEvent::Agent(event) = event { self.handle_event(&event); }
        }
    }

    fn drain_ui_requests(&mut self) {
        while let Ok(request) = self.ui_requests.try_recv() { self.handle_ui_request(request); }
        let closed = self.ui_reply.borrow().as_ref().is_some_and(tokio::sync::oneshot::Sender::is_closed);
        if closed { self.ui_reply.borrow_mut().take(); if let Some(mut dialog) = self.ui_dialog.take() { dialog.dispose(); } }
    }

    fn handle_ui_request(&mut self, request: crate::interactive_extension_ui::UiRequest) {
        use crate::interactive_extension_ui::UiRequest;
        match request {
            UiRequest::HiddenThinkingLabel(label) => { self.hidden_thinking_label = label.unwrap_or_else(|| "Thinking...".into()); for card in &self.assistant_cards { card.borrow_mut().set_hidden_thinking_label(&self.hidden_thinking_label); } }
            UiRequest::Editor { title, prefill, reply } => {
                *self.ui_reply.borrow_mut() = Some(reply);
                let selected = self.ui_reply.clone(); let cancelled = selected.clone();
                self.ui_dialog = Some(Box::new(crate::components::extension_editor::ExtensionEditorComponent::new(&self.theme, self.editor_host.clone(),
                    Arc::new(maho_tui::keybindings::KeybindingsManager::new(maho_core::keybindings::keybindings().clone(), Default::default())), &title, prefill.as_deref(),
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
                let keys = Arc::new(maho_tui::keybindings::KeybindingsManager::new(maho_core::keybindings::keybindings().clone(), Default::default()));
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
            AgentEvent::AgentStart => { self.agent_idle = false; self.pending_tools.clear(); self.working_started_ms = Some(self.clock.elapsed().as_secs_f64() * 1000.0); }
            AgentEvent::AgentEnd { .. } => { self.agent_idle = true; self.pending_tools.clear(); self.tool_reveal.stop(); self.working_started_ms = None; }
            AgentEvent::MessageStart { message } => {
                if message.role() == "user" {
                    let value = serde_json::to_value(message).expect("serializable agent message");
                    let text = value["content"].as_array().map(|parts| parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
                    self.chat.add_child(Rc::new(RefCell::new(UserMessageComponent::new(text, self.theme.clone(), get_markdown_theme(&self.theme), 1, self.markdown_transformers.clone()))));
                } else if message.role() == "assistant" {
                    self.assistant_segments.clear();
                    self.reveal.begin(serde_json::to_value(message).expect("assistant"), self.clock.elapsed().as_secs_f64() * 1000.0);
                    let component = Rc::new(RefCell::new(AssistantMessageComponent::new(None, false, get_markdown_theme(&self.theme), "Thinking…", 1, self.markdown_transformers.clone(), self.theme.clone())));
                    self.chat.add_child(component.clone());
                    self.assistant_cards.push(component.clone());
                    component.borrow_mut().set_hidden_thinking_label(&self.hidden_thinking_label);
                    self.streaming = Some(component);
                } else if message.role() == "custom" { self.add_history_message(message); }
            }
            AgentEvent::MessageUpdate { message, .. } | AgentEvent::MessageEnd { message } if message.role() == "assistant" => {
                if let Some(assistant) = message.as_assistant() {
                    let final_message = matches!(event, AgentEvent::MessageEnd { .. });
                    let mut start = 0;
                    for (index, content) in assistant.content.iter().enumerate() {
                        if let maho_ai::types::ContentBlock::ToolCall(call) = content {
                            self.update_assistant_segment(assistant, start, index, final_message);
                            let args = serde_json::Value::Object(call.arguments.clone());
                            let component = self.tool_component(&call.name, &call.id, args.clone());
                            component.borrow_mut().update_args(args);
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
        let component = if start == 0 { self.streaming.clone() } else {
            Some(self.assistant_segments.entry(start).or_insert_with(|| {
                let component = Rc::new(RefCell::new(AssistantMessageComponent::new(None, false, get_markdown_theme(&self.theme), "Thinking…", 1, self.markdown_transformers.clone(), self.theme.clone())));
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
        for component in &self.tool_cards { component.borrow_mut().tick(now_ms.max(0.0) as u64); }
        if let Some(value) = self.reveal.tick(now_ms) && let Some(component) = &self.streaming { component.borrow_mut().update_content(&value, Some(true)); }
        for (id, value) in self.tool_reveal.tick(now_ms) { if let Some(component) = self.pending_tools.get(&id) { component.borrow_mut().update_result(Self::tool_result(&value, false), true); } }
    }

    pub fn working_frame(&self, now_ms: f64) -> Option<String> {
        if !self.working_visible { return None; }
        let elapsed = (now_ms - self.working_started_ms?).max(0.0);
        let base = |text: &str| self.theme.fg(crate::theme::ThemeColor::Dim, text);
        let glow = |text: &str| self.theme.fg(crate::theme::ThemeColor::Text, text);
        let highlight = |text: &str| self.theme.bold(&glow(text));
        Some(crate::working_status::format_working_status_message_frame(self.working_message.as_deref().unwrap_or("Working"), elapsed / 1000.0,
            &crate::components::keybinding_hints::key_display_text("app.interrupt"), elapsed,
            &crate::working_status::WorkingStatusTextFrameStyle { base:&base, glow:&glow, highlight:&highlight, shimmer:None }, &base))
    }

    fn tool_component(&mut self, name: &str, id: &str, args: serde_json::Value) -> Rc<RefCell<ToolExecutionComponent>> {
        self.pending_tools.entry(id.into()).or_insert_with(|| {
            let component = Rc::new(RefCell::new(ToolExecutionComponent::new(name, id, args, ToolExecutionOptions::default(), None, &self.session.cwd(), ToolExecutionPresentation::Classic, None, self.theme.clone())));
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
        let component = Rc::new(RefCell::new(AssistantMessageComponent::new(Some(serde_json::to_value(message).expect("assistant")), self.reveal.hide_thinking, get_markdown_theme(&self.theme), "Thinking…", 1, self.markdown_transformers.clone(), self.theme.clone())));
        component.borrow_mut().set_expanded(self.tools_expanded);
        component.borrow_mut().set_hidden_thinking_label(&self.hidden_thinking_label);
        self.chat.add_child(component.clone()); self.assistant_cards.push(component);
    }
    fn add_child(&mut self, component: Rc<RefCell<ToolExecutionComponent>>) { self.chat.add_child(component.clone()); self.tool_cards.push(component); }
    fn create_tool(&mut self, name: &str, id: &str, args: &serde_json::Map<String, serde_json::Value>) -> ToolExecutionComponent {
        ToolExecutionComponent::new(name, id, serde_json::Value::Object(args.clone()), ToolExecutionOptions::default(), None, &self.session.cwd(), ToolExecutionPresentation::Classic, None, self.theme.clone())
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
        let mut lines = self.chat.render(width);
        if let Some(frame) = self.working_frame(self.clock.elapsed().as_secs_f64() * 1000.0) { lines.extend(maho_tui::components::text::Text::with_padding(frame, 1, 0).render(width)); }
        if let Some(header) = &mut self.header { let mut top = header.render(width); top.extend(lines); lines = top; }
        for (_, widget, placement) in &mut self.widgets { if *placement == maho_ext_api::WidgetPlacement::AboveEditor { lines.extend(widget.render(width)); } }
        if self.shortcut_overlay { lines.extend(crate::components::shortcut_overlay::ShortcutOverlay::new(&self.theme).render(width)); }
        lines.extend(if let Some(dialog) = &mut self.ui_dialog { dialog.render(width) } else if let Some(input) = &mut self.rename_input { input.render(width) } else { self.editor.render(width) });
        for (_, widget, placement) in &mut self.widgets { if *placement == maho_ext_api::WidgetPlacement::BelowEditor { lines.extend(widget.render(width)); } }
        let mut footer = crate::components::footer::FooterComponent::new(self.footer_snapshot());
        footer.set_auto_compact_enabled(self.session.auto_compaction_enabled());
        footer.snapshot.extension_statuses = self.extension_ui.statuses.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        lines.extend(if let Some(custom) = &mut self.footer { custom.render(width) } else { footer.render(width, &self.theme).expect("footer layout") });
        lines
    }
    fn handle_input(&mut self, data: &str) {
        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system clock").as_millis();
        self.handle_input_at(data, u64::try_from(now_ms).expect("timestamp"));
    }
    fn has_input_handler(&self) -> bool { true }
    fn invalidate(&mut self) { self.chat.invalidate(); self.editor.invalidate(); }
}

impl InteractiveMode {
    fn handle_editor_input(&mut self, data: &str) {
        if let Some(dialog) = &mut self.ui_dialog {
            dialog.handle_input(data);
            if self.ui_reply.borrow().is_none() { self.ui_dialog = None; self.local_dialog_reply = None; }
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
        } else { self.editor.handle_input(data); }
        *self.extension_ui.editor_text.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = self.editor.editor.get_text();
    }
}
