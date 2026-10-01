//! Port of `components/user-message-selector.ts`.
use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::components::spacer::Spacer;
use maho_tui::components::text::Text;
use maho_tui::keybindings::get_keybindings;
use maho_tui::tui::{Component, Container};
use maho_tui::utils::truncate_to_width;

use super::dynamic_border::DynamicBorder;
use crate::theme::{Theme, ThemeColor};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserMessageItem {
    pub id: String,
    pub text: String,
    pub timestamp: Option<String>,
}

pub type SelectCallback = Box<dyn FnMut(&str)>;
pub type CancelCallback = Box<dyn FnMut()>;

pub struct UserMessageList {
    messages: Vec<UserMessageItem>,
    selected_index: usize,
    max_visible: usize,
    pub on_select: Option<SelectCallback>,
    pub on_cancel: Option<CancelCallback>,
    theme: Theme,
}

impl UserMessageList {
    pub fn new(messages: Vec<UserMessageItem>, initial_selected_id: Option<&str>, theme: Theme) -> Self {
        let initial_index = initial_selected_id
            .and_then(|id| messages.iter().position(|message| message.id == id))
            .map(|index| index as i64)
            .unwrap_or(-1);
        let selected_index = if initial_index >= 0 {
            initial_index as usize
        } else {
            messages.len().saturating_sub(1)
        };
        Self { messages, selected_index, max_visible: 10, on_select: None, on_cancel: None, theme }
    }

    pub fn selected_index(&self) -> usize {
        self.selected_index
    }
}

impl Component for UserMessageList {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        if self.messages.is_empty() {
            lines.push(self.theme.fg(ThemeColor::Muted, "  No user messages found"));
            return lines;
        }

        let half = self.max_visible / 2;
        let start_index = self
            .selected_index
            .saturating_sub(half)
            .min(self.messages.len().saturating_sub(self.max_visible));
        let end_index = (start_index + self.max_visible).min(self.messages.len());

        for index in start_index..end_index {
            let message = &self.messages[index];
            let is_selected = index == self.selected_index;
            let normalized = message.text.replace('\n', " ");
            let normalized = normalized.trim();

            let cursor = if is_selected { self.theme.fg(ThemeColor::Accent, "› ") } else { String::from("  ") };
            let max_message_width = width.saturating_sub(2);
            let truncated = truncate_to_width(normalized, max_message_width, "...", false);
            let message_line = if is_selected {
                cursor + &self.theme.bold(&truncated)
            } else {
                cursor + &truncated
            };
            lines.push(message_line);

            let position = index + 1;
            lines.push(self.theme.fg(ThemeColor::Muted, &format!("  Message {position} of {}", self.messages.len())));
            lines.push(String::new());
        }

        if start_index > 0 || end_index < self.messages.len() {
            lines.push(self.theme.fg(
                ThemeColor::Muted,
                &format!("  ({}/{})", self.selected_index + 1, self.messages.len()),
            ));
        }

        lines
    }

    fn handle_input(&mut self, data: &str) {
        let keybindings = get_keybindings();
        if keybindings.matches(data, "tui.select.up") {
            self.selected_index = if self.selected_index == 0 {
                self.messages.len().saturating_sub(1)
            } else {
                self.selected_index - 1
            };
        } else if keybindings.matches(data, "tui.select.down") {
            self.selected_index = if self.selected_index + 1 == self.messages.len() {
                0
            } else {
                self.selected_index + 1
            };
        } else if keybindings.matches(data, "tui.select.confirm") {
            let selected = self.messages.get(self.selected_index).map(|message| message.id.clone());
            if let (Some(id), Some(on_select)) = (selected, self.on_select.as_mut()) {
                on_select(&id);
            }
        } else if keybindings.matches(data, "tui.select.cancel")
            && let Some(on_cancel) = self.on_cancel.as_mut()
        {
            on_cancel();
        }
    }

    fn has_input_handler(&self) -> bool {
        true
    }
}

pub struct UserMessageSelectorComponent {
    content: Container,
    message_list: Rc<RefCell<UserMessageList>>,
}

impl UserMessageSelectorComponent {
    pub fn new(
        messages: Vec<UserMessageItem>,
        on_select: SelectCallback,
        on_cancel: CancelCallback,
        initial_selected_id: Option<&str>,
        theme: Theme,
    ) -> Self {
        let mut content = Container::new();
        content.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        content.add_child(Rc::new(RefCell::new(Text::with_padding(theme.bold("Fork from Message"), 1, 0))));
        content.add_child(Rc::new(RefCell::new(Text::with_padding(
            theme.fg(
                ThemeColor::Muted,
                "Select a user message to copy the active path up to that point into a new session",
            ),
            1,
            0,
        ))));
        content.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        content.add_child(Rc::new(RefCell::new(DynamicBorder::new(theme.clone()))));
        content.add_child(Rc::new(RefCell::new(Spacer::new(1))));

        let mut message_list = UserMessageList::new(messages, initial_selected_id, theme.clone());
        message_list.on_select = Some(on_select);
        message_list.on_cancel = Some(on_cancel);
        let empty = message_list.messages.is_empty();
        let message_list = Rc::new(RefCell::new(message_list));
        content.add_child(Rc::clone(&message_list) as Rc<RefCell<dyn Component>>);

        content.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        content.add_child(Rc::new(RefCell::new(DynamicBorder::new(theme))));

        if empty && let Some(on_cancel) = message_list.borrow_mut().on_cancel.as_mut() {
            on_cancel();
        }

        Self { content, message_list }
    }

    pub fn message_list(&self) -> &Rc<RefCell<UserMessageList>> {
        &self.message_list
    }
}

impl Component for UserMessageSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.content.render(width)
    }
    fn handle_input(&mut self, data: &str) {
        self.message_list.borrow_mut().handle_input(data);
    }
    fn has_input_handler(&self) -> bool {
        true
    }
    fn invalidate(&mut self) {
        self.content.invalidate();
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
