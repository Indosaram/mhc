//! Native user/assistant transcript slice of interactive-mode.ts.
//! Command dispatch, tools, runtime replacement and extension UI remain partial.
use std::{cell::RefCell, rc::Rc, sync::Arc};
use maho_agent::types::AgentEvent;
use maho_core::agent_session::{AgentSession, AgentSessionSubscription, PromptDisposition, PromptOptions};
use maho_tui::tui::{Component, Container};
use crate::{components::{assistant_message::AssistantMessageComponent, user_message::UserMessageComponent, markdown_transform::get_markdown_theme}, theme::Theme};

pub struct InteractiveMode {
    session: Arc<AgentSession>,
    events: tokio::sync::mpsc::UnboundedReceiver<maho_ext_api::AgentSessionEvent>,
    _subscription: AgentSessionSubscription,
    chat: Container,
    streaming: Option<Rc<RefCell<AssistantMessageComponent>>>,
    theme: Theme,
    pub agent_idle: bool,
}

impl InteractiveMode {
    pub fn new(session: Arc<AgentSession>, theme: Theme) -> Self {
        let (sender, events) = tokio::sync::mpsc::unbounded_channel();
        let subscription = session.subscribe(Arc::new(move |event| { drop(sender.send(event.clone())); }));
        Self { session, events, _subscription: subscription, chat: Container::new(), streaming: None, theme, agent_idle: true }
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
            AgentEvent::AgentStart => self.agent_idle = false,
            AgentEvent::AgentEnd { .. } => self.agent_idle = true,
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
                if let Some(component) = &self.streaming {
                    let final_message = matches!(event, AgentEvent::MessageEnd { .. });
                    component.borrow_mut().update_content(&serde_json::to_value(message).expect("serializable agent message"), Some(!final_message));
                    if final_message { self.streaming = None; }
                }
            }
            _ => {}
        }
    }
}

impl Component for InteractiveMode {
    fn render(&mut self, width: usize) -> Vec<String> { self.drain_events(); self.chat.render(width) }
    fn invalidate(&mut self) { self.chat.invalidate(); }
}
