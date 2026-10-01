//! Port of components/extension-selector.ts.
//!
//! Generic selector for extensions: a windowed list of string options with keyboard navigation.
//! senpi's `setInterval` countdown becomes an explicit host `tick()` (plan todo 31 keeps timers
//! host-driven); expiry calls the cancel callback.

use maho_tui::components::text::Text;
use maho_tui::keybindings::KeybindingsManager;
use maho_tui::tui::{Component, Focusable};
use std::sync::Arc;

use super::keybinding_hints::{key_hint, raw_key_hint};
use super::theme_selector::dynamic_border;
use crate::theme::theme::{Theme, ThemeColor};

const DEFAULT_MAX_VISIBLE_OPTIONS: usize = 10;

#[derive(Default)]
pub struct ExtensionSelectorOptions {
    /// senpi's `tui.terminal.rows`; `None` uses the test-double default of 10.
    pub terminal_rows: Option<usize>,
    pub timeout_ms: Option<u64>,
    pub on_toggle_tools_expanded: Option<Box<dyn FnMut()>>,
}

pub struct ExtensionSelectorComponent {
    theme: Theme,
    keybindings: Arc<KeybindingsManager>,
    options: Vec<String>,
    selected_index: usize,
    base_title: String,
    max_visible_options: usize,
    remaining_seconds: Option<i64>,
    disposed: bool,
    on_select: Box<dyn FnMut(&str)>,
    on_cancel: Box<dyn FnMut()>,
    on_toggle_tools_expanded: Option<Box<dyn FnMut()>>,
}

impl ExtensionSelectorComponent {
    pub fn new(
        theme: &Theme,
        keybindings: Arc<KeybindingsManager>,
        title: &str,
        options: Vec<String>,
        on_select: Box<dyn FnMut(&str)>,
        on_cancel: Box<dyn FnMut()>,
        opts: ExtensionSelectorOptions,
    ) -> Self {
        let max_visible_options = opts
            .terminal_rows
            .map_or(DEFAULT_MAX_VISIBLE_OPTIONS, |rows| (rows / 2).max(5));
        // senpi only arms the countdown when a TUI (and therefore a row count) is present.
        let remaining_seconds = match (opts.timeout_ms, opts.terminal_rows) {
            (Some(timeout), Some(_)) if timeout > 0 => {
                Some(i64::try_from(timeout.div_ceil(1000)).unwrap_or(i64::MAX))
            }
            _ => None,
        };
        Self {
            theme: theme.clone(),
            keybindings,
            options,
            selected_index: 0,
            base_title: title.to_owned(),
            max_visible_options,
            remaining_seconds,
            disposed: false,
            on_select,
            on_cancel,
            on_toggle_tools_expanded: opts.on_toggle_tools_expanded,
        }
    }

    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub fn options(&self) -> &[String] {
        &self.options
    }

    pub fn remaining_seconds(&self) -> Option<i64> {
        self.remaining_seconds
    }

    /// Advance the countdown by one second; expiry cancels the selector like senpi's interval.
    pub fn tick(&mut self) {
        if self.disposed {
            return;
        }
        let Some(remaining) = self.remaining_seconds.as_mut() else {
            return;
        };
        *remaining -= 1;
        if *remaining <= 0 {
            self.disposed = true;
            (self.on_cancel)();
        }
    }

    pub fn dispose(&mut self) {
        self.disposed = true;
    }

    fn title_text(&self) -> String {
        let title = match self.remaining_seconds {
            Some(remaining) => format!("{} ({remaining}s)", self.base_title),
            None => self.base_title.clone(),
        };
        self.theme.fg(ThemeColor::Accent, &self.theme.bold(&title))
    }

    fn visible_range(&self) -> (usize, usize) {
        let count = self.options.len();
        let start_index = self
            .selected_index
            .saturating_sub(self.max_visible_options / 2)
            .min(count.saturating_sub(self.max_visible_options));
        let end_index = (start_index + self.max_visible_options).min(count);
        (start_index, end_index)
    }
}

impl Component for ExtensionSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = vec![dynamic_border(&self.theme, ThemeColor::Border, width), String::new()];
        lines.extend(Text::with_padding(self.title_text(), 1, 0).render(width));
        lines.push(String::new());

        let (start_index, end_index) = self.visible_range();
        if start_index > 0 {
            lines.extend(
                Text::with_padding(
                    self.theme.fg(ThemeColor::Muted, &format!("  … {start_index} more above")),
                    1,
                    0,
                )
                .render(width),
            );
        }
        for index in start_index..end_index {
            let text = if index == self.selected_index {
                format!(
                    "{}{}",
                    self.theme.fg(ThemeColor::Accent, "→ "),
                    self.theme.fg(ThemeColor::Accent, &self.options[index])
                )
            } else {
                format!("  {}", self.theme.fg(ThemeColor::Text, &self.options[index]))
            };
            lines.extend(Text::with_padding(text, 1, 0).render(width));
        }
        let count = self.options.len();
        if end_index < count {
            lines.extend(
                Text::with_padding(
                    self.theme.fg(ThemeColor::Muted, &format!("  … {} more below", count - end_index)),
                    1,
                    0,
                )
                .render(width),
            );
        }

        lines.push(String::new());
        let hints = format!(
            "{}  {}  {}",
            raw_key_hint("↑↓", "navigate", &self.theme),
            key_hint("tui.select.confirm", "select", &self.theme),
            key_hint("tui.select.cancel", "cancel", &self.theme)
        );
        lines.extend(Text::with_padding(hints, 1, 0).render(width));
        lines.push(String::new());
        lines.push(dynamic_border(&self.theme, ThemeColor::Border, width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = &self.keybindings;
        if kb.matches(data, "app.tools.expand") {
            if let Some(callback) = &mut self.on_toggle_tools_expanded {
                callback();
            }
        } else if kb.matches(data, "tui.select.up") || data == "k" {
            self.selected_index = self.selected_index.saturating_sub(1);
        } else if kb.matches(data, "tui.select.down") || data == "j" {
            self.selected_index = (self.selected_index + 1).min(self.options.len().saturating_sub(1));
        } else if kb.matches(data, "tui.select.confirm") || data == "\n" {
            if let Some(selected) = self.options.get(self.selected_index) {
                let selected = selected.clone();
                (self.on_select)(&selected);
            }
        } else if kb.matches(data, "tui.select.cancel") {
            (self.on_cancel)();
        }
    }

    fn has_input_handler(&self) -> bool {
        true
    }
}

impl Focusable for ExtensionSelectorComponent {
    fn focused(&self) -> bool {
        false
    }

    fn set_focused(&mut self, _value: bool) {}
}
