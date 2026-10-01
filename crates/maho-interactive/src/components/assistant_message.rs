//! Port of `components/assistant-message.ts`.
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use maho_tui::components::markdown::{MarkdownOptions, MarkdownTheme};
use maho_tui::components::spacer::Spacer;
use maho_tui::components::text::Text;
use maho_tui::tui::{Component, Container, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType};
use serde_json::{json, Value};

use super::assistant_render_descriptors::{
    create_assistant_render_descriptors, AssistantRenderDescriptor, AssistantRenderDescriptorOptions, DescriptorKind,
};
use super::markdown_transform::{create_markdown_transform, get_markdown_theme, MessageType, TransformedMarkdown};
use super::render_signature::create_bounded_render_signature;
use crate::theme::{Theme, ThemeColor};

const OSC133_ZONE_START: &str = "\x1b]133;A\x07";
const OSC133_ZONE_END: &str = "\x1b]133;B\x07";
const OSC133_ZONE_FINAL: &str = "\x1b]133;C\x07";

pub type ThinkingVisibilityOverrides = Vec<(usize, bool)>;

fn overrides_to_value(overrides: &ThinkingVisibilityOverrides) -> Value {
    Value::Array(
        overrides
            .iter()
            .map(|(run, hidden)| Value::Array(vec![json!(run), json!(hidden)]))
            .collect(),
    )
}

struct ThinkingToggle {
    inner: Rc<RefCell<dyn Component>>,
    run_index: usize,
    pending_toggle: Rc<Cell<Option<usize>>>,
}

impl Component for ThinkingToggle {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.inner.borrow_mut().render(width)
    }
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if event.event_type == TuiMouseEventType::Click && event.button == TuiMouseButton::Left {
            self.pending_toggle.set(Some(self.run_index));
            return Some(TuiMouseEventResult { handled: true, ..Default::default() });
        }
        self.inner.borrow_mut().handle_mouse(event)
    }
    fn invalidate(&mut self) {
        self.inner.borrow_mut().invalidate();
    }
    fn dispose(&mut self) {
        self.inner.borrow_mut().dispose();
    }
}

struct RenderCache {
    lines: Vec<String>,
    signature: String,
    width: usize,
}

pub struct AssistantMessageComponent {
    render_cache: Option<RenderCache>,
    content: Container,
    hide_thinking_block: bool,
    markdown_theme: MarkdownTheme,
    hidden_thinking_label: String,
    output_pad: usize,
    markdown_transformers: Vec<super::markdown_transform::MarkdownTransformer>,
    last_message: Option<Value>,
    last_message_signature: Option<String>,
    render_descriptors: Vec<AssistantRenderDescriptor>,
    markdown_slots: Vec<Option<Rc<RefCell<TransformedMarkdown>>>>,
    has_tool_calls: bool,
    expanded: bool,
    provider_error_owned: bool,
    is_streaming: bool,
    thinking_visibility_overrides: ThinkingVisibilityOverrides,
    pending_toggle: Rc<Cell<Option<usize>>>,
    theme: Theme,
}

impl AssistantMessageComponent {
    pub fn new(
        message: Option<Value>,
        hide_thinking_block: bool,
        markdown_theme: MarkdownTheme,
        hidden_thinking_label: &str,
        output_pad: usize,
        markdown_transformers: Vec<super::markdown_transform::MarkdownTransformer>,
        theme: Theme,
    ) -> Self {
        let mut component = Self {
            render_cache: None,
            content: Container::new(),
            hide_thinking_block,
            markdown_theme,
            hidden_thinking_label: hidden_thinking_label.to_owned(),
            output_pad,
            markdown_transformers,
            last_message: None,
            last_message_signature: None,
            render_descriptors: Vec::new(),
            markdown_slots: Vec::new(),
            has_tool_calls: false,
            expanded: false,
            provider_error_owned: false,
            is_streaming: false,
            thinking_visibility_overrides: Vec::new(),
            pending_toggle: Rc::new(Cell::new(None)),
            theme,
        };
        if let Some(message) = message {
            component.update_content(&message, None);
        }
        component
    }

    pub fn set_hide_thinking_block(&mut self, hide: bool) {
        if self.hide_thinking_block == hide {
            return;
        }
        self.hide_thinking_block = hide;
        self.thinking_visibility_overrides.clear();
        self.refresh_content();
    }

    pub fn set_hidden_thinking_label(&mut self, label: &str) {
        if self.hidden_thinking_label == label {
            return;
        }
        self.hidden_thinking_label = label.to_owned();
        self.refresh_content();
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        if self.expanded == expanded {
            return;
        }
        self.expanded = expanded;
        self.refresh_content();
    }

    pub fn set_provider_error_owned(&mut self, owned: bool) {
        if self.provider_error_owned == owned {
            return;
        }
        self.provider_error_owned = owned;
        self.refresh_content();
    }

    pub fn is_exploration_detail(&self) -> bool {
        self.render_descriptors
            .iter()
            .all(|part| matches!(part.kind, DescriptorKind::Spacer | DescriptorKind::ThinkingLabel))
    }

    pub fn set_output_pad(&mut self, padding: usize) {
        self.output_pad = padding;
        self.render_descriptors.clear();
        self.refresh_content();
    }

    pub fn update_content(&mut self, message: &Value, is_streaming: Option<bool>) {
        let is_streaming = is_streaming.unwrap_or(self.is_streaming);
        let streaming_changed = self.is_streaming != is_streaming;
        self.is_streaming = is_streaming;
        self.last_message = Some(message.clone());
        let message_signature = self.create_message_signature(message);
        if !streaming_changed && self.last_message_signature.as_ref() == Some(&message_signature) {
            return;
        }
        self.last_message_signature = Some(message_signature);
        self.render_cache = None;
        if streaming_changed {
            self.render_descriptors.clear();
        }
        self.has_tool_calls = message["content"]
            .as_array()
            .is_some_and(|content| content.iter().any(|part| part["type"] == "toolCall"));
        let descriptors = create_assistant_render_descriptors(
            message,
            &AssistantRenderDescriptorOptions {
                expanded: self.expanded,
                provider_error_owned: self.provider_error_owned,
                hidden_thinking_label: &self.hidden_thinking_label,
                hide_thinking_block: self.hide_thinking_block,
                thinking_visibility_overrides: &self
                    .thinking_visibility_overrides
                    .iter()
                    .copied()
                    .collect::<BTreeMap<usize, bool>>(),
                has_tool_calls: self.has_tool_calls,
            },
            &self.theme,
        );
        self.reconcile_render_descriptors(descriptors);
    }

    fn apply_pending_toggle(&mut self) {
        let Some(run_index) = self.pending_toggle.take() else {
            return;
        };
        let hidden = self
            .thinking_visibility_overrides
            .iter()
            .find(|(run, _)| *run == run_index)
            .map(|(_, hidden)| *hidden)
            .unwrap_or(self.hide_thinking_block);
        match self.thinking_visibility_overrides.iter_mut().find(|(run, _)| *run == run_index) {
            Some(entry) => entry.1 = !hidden,
            None => self.thinking_visibility_overrides.push((run_index, !hidden)),
        }
        self.refresh_content();
    }

    fn reconcile_render_descriptors(&mut self, descriptors: Vec<AssistantRenderDescriptor>) {
        let mut divergent_index = 0;
        let shared_length = self.render_descriptors.len().min(descriptors.len());
        while divergent_index < shared_length {
            let previous = &self.render_descriptors[divergent_index];
            let next = &descriptors[divergent_index];
            if previous.kind != next.kind {
                break;
            }
            if previous.text != next.text {
                let updated = match (&next.kind, self.markdown_slots.get(divergent_index)) {
                    (DescriptorKind::TextMarkdown | DescriptorKind::ThinkingMarkdown, Some(Some(slot))) => {
                        slot.borrow_mut().set_raw(&next.text);
                        true
                    }
                    _ => false,
                };
                if !updated {
                    break;
                }
            }
            divergent_index += 1;
        }
        while self.content.children.len() > divergent_index {
            if let Some(child) = self.content.children.pop() {
                child.borrow_mut().dispose();
            }
        }
        self.markdown_slots.truncate(divergent_index);
        for descriptor in descriptors.iter().skip(divergent_index) {
            let child = self.create_render_child(descriptor);
            self.content.add_child(child);
        }
        self.render_descriptors = descriptors;
    }

    fn create_render_child(&mut self, descriptor: &AssistantRenderDescriptor) -> Rc<RefCell<dyn Component>> {
        match descriptor.kind {
            DescriptorKind::Spacer => {
                self.markdown_slots.push(None);
                Rc::new(RefCell::new(Spacer::new(1)))
            }
            DescriptorKind::TextMarkdown => {
                let transform = Rc::new(create_markdown_transform(
                    MessageType::Assistant,
                    self.is_streaming,
                    self.markdown_transformers.clone(),
                ));
                let markdown = Rc::new(RefCell::new(TransformedMarkdown::new(
                    &descriptor.text,
                    self.output_pad,
                    0,
                    self.markdown_theme.clone(),
                    None,
                    MarkdownOptions::default(),
                    transform,
                )));
                self.markdown_slots.push(Some(Rc::clone(&markdown)));
                self.wrap_thinking_toggle(markdown, descriptor)
            }
            DescriptorKind::ThinkingMarkdown => {
                let theme = self.theme.clone();
                let transform = Rc::new(create_markdown_transform(
                    MessageType::AssistantThinking,
                    self.is_streaming,
                    self.markdown_transformers.clone(),
                ));
                let markdown = Rc::new(RefCell::new(TransformedMarkdown::new(
                    &descriptor.text,
                    self.output_pad,
                    0,
                    self.markdown_theme.clone(),
                    Some(maho_tui::components::markdown::DefaultTextStyle {
                        color: Some(std::sync::Arc::new(move |text| theme.fg(ThemeColor::ThinkingText, text))),
                        italic: true,
                        ..Default::default()
                    }),
                    MarkdownOptions::default(),
                    transform,
                )));
                self.markdown_slots.push(Some(Rc::clone(&markdown)));
                self.wrap_thinking_toggle(markdown, descriptor)
            }
            DescriptorKind::ThinkingLabel => {
                self.markdown_slots.push(None);
                let text = Rc::new(RefCell::new(Text::with_padding(&descriptor.text, self.output_pad, 0)));
                self.wrap_thinking_toggle(text, descriptor)
            }
            DescriptorKind::ErrorText => {
                self.markdown_slots.push(None);
                Rc::new(RefCell::new(Text::with_padding(&descriptor.text, self.output_pad, 0)))
            }
            DescriptorKind::ProviderNativeSummary => {
                self.markdown_slots.push(None);
                Rc::new(RefCell::new(Text::with_padding(&descriptor.text, 1, 0)))
            }
            DescriptorKind::ProviderNativeBody => {
                self.markdown_slots.push(None);
                Rc::new(RefCell::new(Text::with_padding(&descriptor.text, 3, 0)))
            }
        }
    }

    fn wrap_thinking_toggle<T: Component + 'static>(
        &self,
        inner: Rc<RefCell<T>>,
        descriptor: &AssistantRenderDescriptor,
    ) -> Rc<RefCell<dyn Component>> {
        match descriptor.thinking_run {
            None => inner as Rc<RefCell<dyn Component>>,
            Some(run_index) => Rc::new(RefCell::new(ThinkingToggle {
                inner: inner as Rc<RefCell<dyn Component>>,
                run_index,
                pending_toggle: Rc::clone(&self.pending_toggle),
            })),
        }
    }

    fn create_message_signature(&self, message: &Value) -> String {
        create_bounded_render_signature(&json!({
            "content": message["content"].clone(),
            "hiddenThinkingLabel": self.hidden_thinking_label,
            "hideThinkingBlock": self.hide_thinking_block,
            "thinkingVisibilityOverrides": overrides_to_value(&self.thinking_visibility_overrides),
            "errorState": [message["diagnostics"].clone(), message["errorMessage"].clone()],
            "stopReason": message["stopReason"].clone(),
        }))
    }

    fn refresh_content(&mut self) {
        let Some(message) = self.last_message.clone() else {
            return;
        };
        self.last_message_signature = None;
        self.update_content(&message, None);
    }
}

impl Component for AssistantMessageComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.apply_pending_toggle();
        let signature = self.last_message_signature.clone().unwrap_or_default();
        if let Some(cache) = &self.render_cache
            && cache.width == width
            && cache.signature == signature
        {
            return cache.lines.clone();
        }
        let mut lines = self.content.render(width);
        if !self.has_tool_calls && !lines.is_empty() {
            if let Some(first) = lines.first_mut() {
                first.insert_str(0, OSC133_ZONE_START);
            }
            if let Some(last) = lines.last_mut() {
                last.insert_str(0, &format!("{OSC133_ZONE_END}{OSC133_ZONE_FINAL}"));
            }
        }
        self.render_cache = Some(RenderCache { lines: lines.clone(), signature, width });
        lines
    }

    fn invalidate(&mut self) {
        self.render_cache = None;
        self.content.invalidate();
        self.render_descriptors.clear();
        self.markdown_slots.clear();
        self.refresh_content();
    }

    fn dispose(&mut self) {
        self.content.dispose();
    }

    fn as_container(&self) -> Option<&Container> {
        Some(&self.content)
    }

    fn as_container_mut(&mut self) -> Option<&mut Container> {
        Some(&mut self.content)
    }
}

pub fn default_markdown_theme(theme: &Theme) -> MarkdownTheme {
    get_markdown_theme(theme)
}
