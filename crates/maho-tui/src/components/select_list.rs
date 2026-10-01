//! Port of senpi `packages/tui/src/components/select-list.ts`.

use crate::keybindings::get_keybindings;
use crate::tui::{Component, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType};
use crate::utils::{truncate_to_width, visible_width};

const DEFAULT_PRIMARY_COLUMN_WIDTH: usize = 32;
const PRIMARY_COLUMN_GAP: usize = 2;
const MIN_DESCRIPTION_WIDTH: usize = 10;

fn normalize_to_single_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_break = false;
    for ch in text.chars() {
        if ch == '\r' || ch == '\n' {
            if !last_was_break {
                out.push(' ');
            }
            last_was_break = true;
        } else {
            out.push(ch);
            last_was_break = false;
        }
    }
    out.trim().to_string()
}

fn clamp_usize(value: usize, min: usize, max: usize) -> usize {
    value.max(min).min(max)
}

#[derive(Debug, Clone)]
pub struct SelectItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// Decomposed parts of a single select-list row, handed to a custom row composer.
pub struct SelectListRowParts {
    /// The selection prefix exactly as rendered ("-> " selected, "  " not); for selected rows
    /// the theme's `selected_prefix` has already been applied.
    pub prefix: String,
    pub primary: String,
    /// The truncated description including column-alignment spacing. `None` when the row
    /// renders no description column.
    pub description: Option<String>,
    pub is_selected: bool,
}

pub type SelectListRenderRow = std::rc::Rc<dyn Fn(&SelectListRowParts) -> String>;

pub struct SelectListTheme {
    pub selected_prefix: std::rc::Rc<dyn Fn(&str) -> String>,
    pub selected_text: std::rc::Rc<dyn Fn(&str) -> String>,
    pub description: std::rc::Rc<dyn Fn(&str) -> String>,
    pub scroll_info: std::rc::Rc<dyn Fn(&str) -> String>,
    pub no_match: std::rc::Rc<dyn Fn(&str) -> String>,
    /// Optional row composer taking over rendering of every item row. When absent, rendering
    /// is byte-identical to the legacy composition (prefix + primary wrapped in
    /// `selected_text`, etc.).
    pub render_row: Option<SelectListRenderRow>,
}

pub struct SelectListTruncatePrimaryContext<'a> {
    pub text: String,
    pub max_width: usize,
    pub column_width: usize,
    pub item: &'a SelectItem,
    pub is_selected: bool,
}

pub type TruncatePrimaryFn = std::rc::Rc<dyn Fn(&SelectListTruncatePrimaryContext<'_>) -> String>;

#[derive(Default)]
pub struct SelectListLayoutOptions {
    pub min_primary_column_width: Option<usize>,
    pub max_primary_column_width: Option<usize>,
    pub truncate_primary: Option<TruncatePrimaryFn>,
}

pub type OnSelect = Box<dyn FnMut(&SelectItem)>;
pub type OnCancel = Box<dyn FnMut()>;

pub struct SelectList {
    items: Vec<SelectItem>,
    filtered_items: Vec<SelectItem>,
    selected_index: usize,
    mouse_pressed_index: Option<usize>,
    max_visible: usize,
    theme: SelectListTheme,
    layout: SelectListLayoutOptions,
    pub on_select: Option<OnSelect>,
    pub on_cancel: Option<OnCancel>,
    pub on_selection_change: Option<OnSelect>,
}

impl SelectList {
    pub fn new(items: Vec<SelectItem>, max_visible: usize, theme: SelectListTheme, layout: SelectListLayoutOptions) -> Self {
        Self {
            filtered_items: items.clone(),
            items,
            selected_index: 0,
            mouse_pressed_index: None,
            max_visible,
            theme,
            layout,
            on_select: None,
            on_cancel: None,
            on_selection_change: None,
        }
    }

    pub fn set_filter(&mut self, filter: &str) {
        let filter_lower = filter.to_lowercase();
        self.filtered_items = self
            .items
            .iter()
            .filter(|item| item.value.to_lowercase().starts_with(&filter_lower))
            .cloned()
            .collect();
        self.selected_index = 0;
    }

    pub fn set_selected_index(&mut self, index: usize) {
        self.selected_index = index.min(self.filtered_items.len().saturating_sub(1));
    }

    fn get_visible_range(&self) -> (usize, usize) {
        let len = self.filtered_items.len();
        let start_index = (self.selected_index as i64 - (self.max_visible / 2) as i64)
            .min(len as i64 - self.max_visible as i64)
            .max(0) as usize;
        let end_index = (start_index + self.max_visible).min(len);
        (start_index, end_index)
    }

    fn get_display_value<'a>(&self, item: &'a SelectItem) -> &'a str {
        if item.label.is_empty() {
            &item.value
        } else {
            &item.label
        }
    }

    fn get_primary_column_bounds(&self) -> (usize, usize) {
        let raw_min = self
            .layout
            .min_primary_column_width
            .or(self.layout.max_primary_column_width)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);
        let raw_max = self
            .layout
            .max_primary_column_width
            .or(self.layout.min_primary_column_width)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);
        (raw_min.min(raw_max).max(1), raw_min.max(raw_max).max(1))
    }

    fn get_primary_column_width(&self) -> usize {
        let (min, max) = self.get_primary_column_bounds();
        let widest_primary = self
            .filtered_items
            .iter()
            .fold(0usize, |widest, item| widest.max(visible_width(self.get_display_value(item)) + PRIMARY_COLUMN_GAP));
        clamp_usize(widest_primary, min, max)
    }

    fn truncate_primary(&self, item: &SelectItem, is_selected: bool, max_width: usize, column_width: usize) -> String {
        let display_value = self.get_display_value(item).to_string();
        let truncated_value = match &self.layout.truncate_primary {
            Some(f) => f(&SelectListTruncatePrimaryContext {
                text: display_value.clone(),
                max_width,
                column_width,
                item,
                is_selected,
            }),
            None => truncate_to_width(&display_value, max_width, "", false),
        };
        truncate_to_width(&truncated_value, max_width, "", false)
    }

    fn compose_row(&self, prefix: &str, primary: &str, description: Option<String>, is_selected: bool) -> String {
        if let Some(render_row) = &self.theme.render_row {
            let prefix = if is_selected { (self.theme.selected_prefix)(prefix) } else { prefix.to_string() };
            return render_row(&SelectListRowParts {
                prefix,
                primary: primary.to_string(),
                description,
                is_selected,
            });
        }
        if is_selected {
            return (self.theme.selected_text)(&format!("{prefix}{primary}{}", description.as_deref().unwrap_or("")));
        }
        match description {
            Some(description) => format!("{prefix}{primary}{}", (self.theme.description)(&description)),
            None => format!("{prefix}{primary}"),
        }
    }

    fn render_item(
        &self,
        item: &SelectItem,
        is_selected: bool,
        width: usize,
        description_single_line: Option<&str>,
        primary_column_width: usize,
    ) -> String {
        let prefix = if is_selected { "\u{2192} " } else { "  " };
        let prefix_width = visible_width(prefix);

        if let Some(description_single_line) = description_single_line
            && width > 40
        {
            let effective_primary_column_width =
                clamp_usize(primary_column_width, 1, width.saturating_sub(prefix_width).saturating_sub(4));
            let max_primary_width = effective_primary_column_width.saturating_sub(PRIMARY_COLUMN_GAP).max(1);
            let truncated_value =
                self.truncate_primary(item, is_selected, max_primary_width, effective_primary_column_width);
            let truncated_value_width = visible_width(&truncated_value);
            let spacing = " ".repeat(effective_primary_column_width.saturating_sub(truncated_value_width).max(1));
            let description_start = prefix_width + truncated_value_width + spacing.len();
            let remaining_width = (width as i64) - (description_start as i64) - 2;

            if remaining_width > MIN_DESCRIPTION_WIDTH as i64 {
                let truncated_desc = truncate_to_width(description_single_line, remaining_width as usize, "", false);
                return self.compose_row(prefix, &truncated_value, Some(format!("{spacing}{truncated_desc}")), is_selected);
            }
        }

        let max_width = width.saturating_sub(prefix_width).saturating_sub(2);
        let truncated_value = self.truncate_primary(item, is_selected, max_width, max_width);
        self.compose_row(prefix, &truncated_value, None, is_selected)
    }

    fn notify_selection_change(&mut self) {
        if let Some(selected_item) = self.filtered_items.get(self.selected_index).cloned()
            && let Some(on_selection_change) = &mut self.on_selection_change
        {
            on_selection_change(&selected_item);
        }
    }

    pub fn get_selected_item(&self) -> Option<&SelectItem> {
        self.filtered_items.get(self.selected_index)
    }
}

impl Component for SelectList {
    fn invalidate(&mut self) {}

    fn render(&mut self, width: usize) -> Vec<String> {
        if self.filtered_items.is_empty() {
            return vec![(self.theme.no_match)("  No matching commands")];
        }

        let primary_column_width = self.get_primary_column_width();
        let (start_index, end_index) = self.get_visible_range();
        let mut lines = Vec::new();

        for i in start_index..end_index {
            let Some(item) = self.filtered_items.get(i).cloned() else {
                continue;
            };
            let is_selected = i == self.selected_index;
            let description_single_line = item.description.as_deref().map(normalize_to_single_line);
            lines.push(self.render_item(&item, is_selected, width, description_single_line.as_deref(), primary_column_width));
        }

        if start_index > 0 || end_index < self.filtered_items.len() {
            let scroll_text = format!("  ({}/{})", self.selected_index + 1, self.filtered_items.len());
            lines.push((self.theme.scroll_info)(&truncate_to_width(&scroll_text, width.saturating_sub(2), "", false)));
        }

        lines
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if self.filtered_items.is_empty() {
            return None;
        }
        if event.event_type == TuiMouseEventType::Wheel
            && let Some(wheel_delta) = event.wheel_delta
        {
            let delta: i64 = if wheel_delta < 0 { -1 } else { 1 };
            let previous_index = self.selected_index;
            self.selected_index =
                ((self.selected_index as i64 + delta).max(0) as usize).min(self.filtered_items.len() - 1);
            let changed = self.selected_index != previous_index;
            if changed {
                self.notify_selection_change();
            }
            return Some(TuiMouseEventResult { handled: true, capture: false, focus: false, render: Some(changed) });
        }
        if event.button != TuiMouseButton::Left
            || !matches!(event.event_type, TuiMouseEventType::Press | TuiMouseEventType::Click)
        {
            return None;
        }
        let (start_index, end_index) = self.get_visible_range();
        if event.y < 0 {
            return None;
        }
        let item_index = start_index + event.y as usize;
        if item_index < start_index || item_index >= end_index {
            return None;
        }

        if event.event_type == TuiMouseEventType::Press {
            self.mouse_pressed_index = Some(item_index);
            if self.selected_index != item_index {
                self.selected_index = item_index;
                self.notify_selection_change();
            }
            return Some(TuiMouseEventResult { handled: true, capture: false, focus: true, render: None });
        }
        if event.event_type == TuiMouseEventType::Click {
            let clicked_index = self.mouse_pressed_index.unwrap_or(item_index);
            self.mouse_pressed_index = None;
            let changed = self.selected_index != clicked_index;
            self.selected_index = clicked_index;
            if changed {
                self.notify_selection_change();
            }
            if let Some(selected_item) = self.filtered_items.get(self.selected_index).cloned()
                && let Some(on_select) = &mut self.on_select
            {
                on_select(&selected_item);
            }
            return Some(TuiMouseEventResult { handled: true, capture: false, focus: false, render: None });
        }
        None
    }

    fn handle_input(&mut self, key_data: &str) {
        let kb = get_keybindings();
        if kb.matches(key_data, "tui.select.up") {
            self.selected_index =
                if self.selected_index == 0 { self.filtered_items.len() - 1 } else { self.selected_index - 1 };
            self.notify_selection_change();
        } else if kb.matches(key_data, "tui.select.down") {
            self.selected_index =
                if self.selected_index == self.filtered_items.len() - 1 { 0 } else { self.selected_index + 1 };
            self.notify_selection_change();
        } else if kb.matches(key_data, "tui.select.confirm") {
            if let Some(selected_item) = self.filtered_items.get(self.selected_index).cloned()
                && let Some(on_select) = &mut self.on_select
            {
                on_select(&selected_item);
            }
        } else if kb.matches(key_data, "tui.select.cancel")
            && let Some(on_cancel) = &mut self.on_cancel
        {
            on_cancel();
        }
    }

    fn has_input_handler(&self) -> bool {
        true
    }
}
