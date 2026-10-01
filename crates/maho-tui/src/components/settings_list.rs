//! Port of senpi `packages/tui/src/components/settings-list.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use super::input::{Input, InputOptions};
use crate::fuzzy::fuzzy_filter;
use crate::keybindings::get_keybindings;
use crate::tui::{Component, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType};
use crate::utils::{truncate_to_width, visible_width, wrap_text_with_ansi};

/// Called by a submenu when it closes: `selected_value` commits a new value for the item
/// that opened it, `navigate_to` moves the cursor to another item (and reopens its submenu)
/// after close.
pub type SubmenuDone = Rc<dyn Fn(Option<String>, Option<String>)>;
pub type SubmenuFactory = Rc<dyn Fn(&str, SubmenuDone) -> Rc<RefCell<dyn Component>>>;

#[derive(Clone)]
pub struct SettingItem {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub current_value: String,
    /// If provided, Enter/Space cycles through these values.
    pub values: Option<Vec<String>>,
    /// If provided, Enter opens this submenu.
    pub submenu: Option<SubmenuFactory>,
}

pub type LabelStyle = std::rc::Rc<dyn Fn(&str, bool) -> String>;

pub struct SettingsListTheme {
    pub label: LabelStyle,
    pub value: LabelStyle,
    pub description: std::rc::Rc<dyn Fn(&str) -> String>,
    pub cursor: String,
    pub hint: std::rc::Rc<dyn Fn(&str) -> String>,
}

#[derive(Default)]
pub struct SettingsListOptions {
    pub enable_search: bool,
}

pub type OnChange = Box<dyn FnMut(&str, &str)>;

pub struct SettingsList {
    items: Vec<SettingItem>,
    filtered_items: Vec<SettingItem>,
    theme: SettingsListTheme,
    selected_index: usize,
    mouse_pressed_index: Option<usize>,
    max_visible: usize,
    pub on_change: Option<OnChange>,
    pub on_cancel: Option<crate::components::select_list::OnCancel>,
    search_input: Option<Input>,
    search_enabled: bool,
    submenu_component: Option<Rc<RefCell<dyn Component>>>,
    submenu_item_index: Option<usize>,
    navigate_after_close: Rc<RefCell<Option<String>>>,
    pending_commit: Rc<RefCell<Option<(usize, String)>>>,
}

impl SettingsList {
    pub fn new(items: Vec<SettingItem>, max_visible: usize, theme: SettingsListTheme, options: SettingsListOptions) -> Self {
        let search_input = if options.enable_search { Some(Input::new(InputOptions::default())) } else { None };
        Self {
            filtered_items: items.clone(),
            items,
            theme,
            selected_index: 0,
            mouse_pressed_index: None,
            max_visible,
            on_change: None,
            on_cancel: None,
            search_input,
            search_enabled: options.enable_search,
            submenu_component: None,
            submenu_item_index: None,
            navigate_after_close: Rc::new(RefCell::new(None)),
            pending_commit: Rc::new(RefCell::new(None)),
        }
    }

    pub fn update_value(&mut self, id: &str, new_value: &str) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.current_value = new_value.to_string();
        }
    }

    pub fn select_item(&mut self, id: &str) {
        let items = if self.search_enabled { &self.filtered_items } else { &self.items };
        if let Some(index) = items.iter().position(|item| item.id == id) {
            self.selected_index = index;
        }
    }

    fn get_display_items(&self) -> &[SettingItem] {
        if self.search_enabled {
            &self.filtered_items
        } else {
            &self.items
        }
    }

    fn get_visible_range(&self, display_items_len: usize) -> (usize, usize) {
        let start_index = (self.selected_index as i64 - (self.max_visible / 2) as i64)
            .min(display_items_len as i64 - self.max_visible as i64)
            .max(0) as usize;
        let end_index = (start_index + self.max_visible).min(display_items_len);
        (start_index, end_index)
    }

    fn add_hint_line(&self, lines: &mut Vec<String>, width: usize) {
        lines.push(String::new());
        let text = if self.search_enabled {
            "  Type to search \u{b7} Enter/Space to change \u{b7} Esc to cancel"
        } else {
            "  Enter/Space to change \u{b7} Esc to cancel"
        };
        lines.push(truncate_to_width(&(self.theme.hint)(text), width, "", false));
    }

    fn render_main_list(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();

        if self.search_enabled {
            if let Some(search_input) = &mut self.search_input {
                lines.extend(search_input.render(width));
            }
            lines.push(String::new());
        }

        if self.items.is_empty() {
            lines.push((self.theme.hint)("  No settings available"));
            if self.search_enabled {
                self.add_hint_line(&mut lines, width);
            }
            return lines;
        }

        let display_items: Vec<SettingItem> = self.get_display_items().to_vec();
        if display_items.is_empty() {
            lines.push(truncate_to_width(&(self.theme.hint)("  No matching settings"), width, "", false));
            self.add_hint_line(&mut lines, width);
            return lines;
        }

        let (start_index, end_index) = self.get_visible_range(display_items.len());
        let max_label_width = self.items.iter().map(|item| visible_width(&item.label)).max().unwrap_or(0).min(36);

        for i in start_index..end_index {
            let Some(item) = display_items.get(i) else {
                continue;
            };
            let is_selected = i == self.selected_index;
            let prefix = if is_selected { self.theme.cursor.clone() } else { "  ".to_string() };
            let prefix_width = visible_width(&prefix);

            let label_padded = format!("{}{}", item.label, " ".repeat(max_label_width.saturating_sub(visible_width(&item.label))));
            let label_text = (self.theme.label)(&label_padded, is_selected);

            let separator = "  ";
            let used_width = prefix_width + max_label_width + visible_width(separator);
            let value_max_width = (width as i64) - (used_width as i64) - 2;
            let value_text = (self.theme.value)(
                &truncate_to_width(&item.current_value, value_max_width.max(0) as usize, "", false),
                is_selected,
            );

            lines.push(truncate_to_width(&format!("{prefix}{label_text}{separator}{value_text}"), width, "", false));
        }

        if start_index > 0 || end_index < display_items.len() {
            let scroll_text = format!("  ({}/{})", self.selected_index + 1, display_items.len());
            lines.push((self.theme.hint)(&truncate_to_width(&scroll_text, width.saturating_sub(2), "", false)));
        }

        if let Some(selected_item) = display_items.get(self.selected_index)
            && let Some(description) = &selected_item.description
        {
            lines.push(String::new());
            let wrapped_desc = wrap_text_with_ansi(description, width.saturating_sub(4));
            for line in wrapped_desc {
                lines.push((self.theme.description)(&format!("  {line}")));
            }
        }

        self.add_hint_line(&mut lines, width);
        lines
    }

    fn activate_item(&mut self) {
        let Some(item) = self.get_display_items().get(self.selected_index).cloned() else {
            return;
        };

        if let Some(submenu) = item.submenu.clone() {
            self.submenu_item_index = Some(self.selected_index);
            let item_id = item.id.clone();
            let navigate_after_close = Rc::clone(&self.navigate_after_close);
            let pending_commit = Rc::clone(&self.pending_commit);
            let selected_index = self.selected_index;
            let done: SubmenuDone = Rc::new(move |selected_value, navigate_to| {
                if let Some(selected_value) = selected_value {
                    *pending_commit.borrow_mut() = Some((selected_index, selected_value));
                }
                if let Some(navigate_to) = navigate_to {
                    *navigate_after_close.borrow_mut() = Some(navigate_to);
                }
            });
            self.submenu_component = Some(submenu(&item.current_value, done));
            let _ = item_id;
        } else if let Some(values) = &item.values
            && !values.is_empty()
        {
            let current_index = values.iter().position(|v| v == &item.current_value).map(|i| i as i64).unwrap_or(-1);
            let next_index = ((current_index + 1) as usize) % values.len();
            let new_value = values[next_index].clone();
            let item_id = item.id.clone();
            if let Some(stored) = self.items.iter_mut().find(|i| i.id == item_id) {
                stored.current_value = new_value.clone();
            }
            if let Some(stored) = self.filtered_items.iter_mut().find(|i| i.id == item_id) {
                stored.current_value = new_value.clone();
            }
            if let Some(on_change) = &mut self.on_change {
                on_change(&item_id, &new_value);
            }
        }
    }

    fn close_submenu(&mut self) {
        self.submenu_component = None;
        if let Some((index, value)) = self.pending_commit.borrow_mut().take()
            && let Some(item) = self.get_display_items().get(index).cloned()
        {
            if let Some(stored) = self.items.iter_mut().find(|i| i.id == item.id) {
                stored.current_value = value.clone();
            }
            if let Some(stored) = self.filtered_items.iter_mut().find(|i| i.id == item.id) {
                stored.current_value = value.clone();
            }
            if let Some(on_change) = &mut self.on_change {
                on_change(&item.id, &value);
            }
        }
        let navigate_to = self.navigate_after_close.borrow_mut().take();
        if let Some(id) = navigate_to {
            self.submenu_item_index = None;
            self.select_item(&id);
            self.activate_item();
        } else if let Some(submenu_item_index) = self.submenu_item_index {
            self.selected_index = submenu_item_index;
            self.submenu_item_index = None;
        }
    }

    fn apply_filter(&mut self, query: &str) {
        self.filtered_items = fuzzy_filter(&self.items, query, |item| item.label.clone());
        self.selected_index = 0;
    }

    fn take_pending_commit_and_close_if_needed(&mut self) {
        if self.pending_commit.borrow().is_some() || self.navigate_after_close.borrow().is_some() {
            self.close_submenu();
        }
    }
}

impl Component for SettingsList {
    fn invalidate(&mut self) {
        if let Some(submenu) = &self.submenu_component {
            submenu.borrow_mut().invalidate();
        }
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        if let Some(submenu) = self.submenu_component.clone() {
            return submenu.borrow_mut().render(width);
        }
        self.render_main_list(width)
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if let Some(submenu) = self.submenu_component.clone() {
            let result = submenu.borrow_mut().handle_mouse(event);
            self.take_pending_commit_and_close_if_needed();
            return result.map(|mut r| {
                r.focus = true;
                r
            });
        }

        if self.search_enabled {
            if event.y == 0
                && let Some(search_input) = &mut self.search_input
            {
                let result = search_input.handle_mouse(event);
                return result.map(|mut r| {
                    r.focus = true;
                    r
                });
            }
            if event.y == 1 {
                return None;
            }
        }

        let display_items_len = self.get_display_items().len();
        if display_items_len == 0 {
            return None;
        }
        if event.event_type == TuiMouseEventType::Wheel
            && let Some(wheel_delta) = event.wheel_delta
        {
            let delta: i64 = if wheel_delta < 0 { -1 } else { 1 };
            let previous_index = self.selected_index;
            self.selected_index = ((self.selected_index as i64 + delta).max(0) as usize).min(display_items_len - 1);
            let changed = self.selected_index != previous_index;
            return Some(TuiMouseEventResult { handled: true, capture: false, focus: false, render: Some(changed) });
        }
        if event.button != TuiMouseButton::Left
            || !matches!(event.event_type, TuiMouseEventType::Press | TuiMouseEventType::Click)
        {
            return None;
        }

        let row_offset: i64 = if self.search_enabled { 2 } else { 0 };
        let (start_index, end_index) = self.get_visible_range(display_items_len);
        let item_index = start_index as i64 + event.y - row_offset;
        if item_index < start_index as i64 || item_index >= end_index as i64 {
            return None;
        }
        let item_index = item_index as usize;
        if event.event_type == TuiMouseEventType::Press {
            self.mouse_pressed_index = Some(item_index);
            self.selected_index = item_index;
            return Some(TuiMouseEventResult { handled: true, capture: false, focus: true, render: None });
        }
        if event.event_type == TuiMouseEventType::Click {
            self.selected_index = self.mouse_pressed_index.unwrap_or(item_index);
            self.mouse_pressed_index = None;
            self.activate_item();
            return Some(TuiMouseEventResult { handled: true, capture: false, focus: false, render: None });
        }
        None
    }

    fn handle_input(&mut self, data: &str) {
        if let Some(submenu) = self.submenu_component.clone() {
            submenu.borrow_mut().handle_input(data);
            self.take_pending_commit_and_close_if_needed();
            return;
        }

        let kb = get_keybindings();
        let display_items_len = self.get_display_items().len();
        if kb.matches(data, "tui.select.up") {
            if display_items_len == 0 {
                return;
            }
            self.selected_index = if self.selected_index == 0 { display_items_len - 1 } else { self.selected_index - 1 };
        } else if kb.matches(data, "tui.select.down") {
            if display_items_len == 0 {
                return;
            }
            self.selected_index = if self.selected_index == display_items_len - 1 { 0 } else { self.selected_index + 1 };
        } else if kb.matches(data, "tui.select.confirm")
            || (data == " "
                && (!self.search_enabled || self.search_input.as_ref().is_some_and(|input| input.get_value().is_empty())))
        {
            self.activate_item();
        } else if kb.matches(data, "tui.select.cancel") {
            if let Some(on_cancel) = &mut self.on_cancel {
                on_cancel();
            }
        } else if self.search_enabled {
            let value = if let Some(search_input) = &mut self.search_input {
                search_input.handle_input(data);
                Some(search_input.get_value().to_string())
            } else {
                None
            };
            if let Some(value) = value {
                self.apply_filter(&value);
            }
        }
    }

    fn has_input_handler(&self) -> bool {
        true
    }
}
