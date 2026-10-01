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
    pending_tools: BTreeMap<String, Rc<RefCell<ToolExecutionComponent>>>,
    theme: Theme,
    pub editor: CustomEditor,
    submissions: Rc<RefCell<std::collections::VecDeque<String>>>,
    pub agent_idle: bool,
}

impl InteractiveMode {
    pub fn new(session: Arc<AgentSession>, theme: Theme, host: Rc<dyn maho_tui::components::editor::EditorTuiHost>) -> Self {
        let (sender, events) = tokio::sync::mpsc::unbounded_channel();
        let subscription = session.subscribe(Arc::new(move |event| { drop(sender.send(event.clone())); }));
        let submissions = Rc::new(RefCell::new(std::collections::VecDeque::new()));
        let captured = submissions.clone();
        let keys = Arc::new(maho_tui::keybindings::KeybindingsManager::new(maho_core::keybindings::keybindings().clone(), Default::default()));
        let mut editor = CustomEditor::new(host, editor_theme(&theme), keys, CustomEditorOptions::default());
        editor.editor.on_submit = Some(Box::new(move |text| { if !text.trim().is_empty() { captured.borrow_mut().push_back(text.trim().into()); } }));
        Self { session, events, _subscription: subscription, chat: Container::new(), streaming: None, pending_tools: BTreeMap::new(), theme, editor, submissions, agent_idle: true }
    }

    pub async fn submit_editor(&mut self) -> Result<Option<PromptDisposition>, String> {
        let text = self.submissions.borrow_mut().pop_front();
        let Some(text) = text else { return Ok(None); };
        self.editor.editor.add_to_history(&text);
        let options = PromptOptions { streaming_behavior: Some(maho_ext_api::StreamingBehavior::Steer), ..Default::default() };
        self.submit(&text, options).await.map(Some)
    }

    pub async fn submit(&mut self, text: &str, options: PromptOptions) -> Result<PromptDisposition, String> {
        let session = self.session.clone();
        let prompt = session.prompt(text, options);
        tokio::pin!(prompt);
        let result = loop {
            tokio::select! {
                result = &mut prompt => break result,
                Some(event) = self.events.recv() => {
                    if let maho_ext_api::AgentSessionEvent::Agent(event) = event { self.handle_event(&event); }
                }
            }
        };
        self.drain_events();
        result
    }

    pub async fn abort(&self) { self.session.abort().await; }

    pub async fn steer(&self, text: &str) -> Result<(), String> { self.session.steer(text, None, Default::default()).await }

    pub async fn follow_up(&self, text: &str) -> Result<(), String> { self.session.follow_up(text, None, Default::default()).await }

    pub fn drain_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            if let maho_ext_api::AgentSessionEvent::Agent(event) = event { self.handle_event(&event); }
        }
    }

    pub fn handle_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::AgentStart => { self.agent_idle = false; self.pending_tools.clear(); }
            AgentEvent::AgentEnd { .. } => { self.agent_idle = true; self.pending_tools.clear(); }
            AgentEvent::MessageStart { message } => {
                if message.role() == "user" {
                    let value = serde_json::to_value(message).expect("serializable agent message");
                    let text = value["content"].as_array().map(|parts| parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
                    self.chat.add_child(Rc::new(RefCell::new(UserMessageComponent::new(text, self.theme.clone(), get_markdown_theme(&self.theme), 1, Vec::new()))));
                } else if message.role() == "assistant" {
                    let component = Rc::new(RefCell::new(AssistantMessageComponent::new(None, false, get_markdown_theme(&self.theme), "Thinking…", 1, Vec::new(), self.theme.clone())));
                    self.chat.add_child(component.clone());
                    self.streaming = Some(component);
                }
            }
            AgentEvent::MessageUpdate { message, .. } | AgentEvent::MessageEnd { message } if message.role() == "assistant" => {
                if let Some(assistant) = message.as_assistant() {
                    for content in &assistant.content {
                        if let maho_ai::types::ContentBlock::ToolCall(call) = content {
                            let args = serde_json::Value::Object(call.arguments.clone());
                            let component = self.tool_component(&call.name, &call.id, args.clone());
                            component.borrow_mut().update_args(args);
                            if matches!(event, AgentEvent::MessageEnd { .. }) { component.borrow_mut().set_args_complete(); }
                        }
                    }
                    if matches!(event, AgentEvent::MessageEnd { .. }) && matches!(assistant.stop_reason, maho_ai::types::StopReason::Aborted | maho_ai::types::StopReason::Error) {
                        for component in self.pending_tools.values() {
                            component.borrow_mut().update_result(ToolExecutionResult { content: vec![maho_tools::definition::ToolContent::text(assistant.error_message.as_deref().unwrap_or("Error"))], details: None, is_error: true }, false);
                        }
                        self.pending_tools.clear();
                    }
                }
                if let Some(component) = &self.streaming {
                    let final_message = matches!(event, AgentEvent::MessageEnd { .. });
                    component.borrow_mut().update_content(&serde_json::to_value(message).expect("serializable agent message"), Some(!final_message));
                    if final_message { self.streaming = None; }
                }
            }
            AgentEvent::ToolExecutionStart { tool_call_id, tool_name, args } => {
                let component = self.tool_component(tool_name, tool_call_id, args.clone());
                let mut component = component.borrow_mut();
                component.update_args(args.clone());
                component.mark_execution_started();
            }
            AgentEvent::ToolExecutionUpdate { tool_call_id, partial_result, .. } => {
                if let Some(component) = self.pending_tools.get(tool_call_id) { component.borrow_mut().update_result(Self::tool_result(partial_result, false), true); }
            }
            AgentEvent::ToolExecutionEnd { tool_call_id, tool_name, result, is_error } => {
                let component = self.tool_component(tool_name, tool_call_id, serde_json::json!({}));
                component.borrow_mut().update_result(Self::tool_result(result, *is_error), false);
                self.pending_tools.remove(tool_call_id);
            }
            _ => {}
        }
    }

    fn tool_component(&mut self, name: &str, id: &str, args: serde_json::Value) -> Rc<RefCell<ToolExecutionComponent>> {
        self.pending_tools.entry(id.into()).or_insert_with(|| {
            let component = Rc::new(RefCell::new(ToolExecutionComponent::new(name, id, args, ToolExecutionOptions::default(), None, &self.session.cwd(), ToolExecutionPresentation::Classic, None, self.theme.clone())));
            self.chat.add_child(component.clone());
            component
        }).clone()
    }

    fn tool_result(value: &serde_json::Value, is_error: bool) -> ToolExecutionResult {
        ToolExecutionResult { content: serde_json::from_value(value["content"].clone()).expect("typed tool result content"), details: value.get("details").cloned(), is_error }
    }
}

impl Component for InteractiveMode {
    fn render(&mut self, width: usize) -> Vec<String> { self.drain_events(); let mut lines = self.chat.render(width); lines.extend(self.editor.render(width)); lines }
    fn handle_input(&mut self, data: &str) { self.editor.handle_input(data); }
    fn has_input_handler(&self) -> bool { true }
    fn invalidate(&mut self) { self.chat.invalidate(); self.editor.invalidate(); }
}
