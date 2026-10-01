//! Port of components/extension-input.ts.
//!
//! Single-line text input dialog for extensions. senpi's `setInterval` countdown is host-driven
//! here: call `tick()` once per second.

use maho_tui::components::input::{Input, InputOptions};
use maho_tui::components::text::Text;
use maho_tui::keybindings::get_keybindings;
use maho_tui::tui::{Component, Focusable};

use super::keybinding_hints::key_hint;
use super::theme_selector::dynamic_border;
use crate::theme::theme::{Theme, ThemeColor};

#[derive(Default)]
pub struct ExtensionInputOptions {
    /// senpi only arms the countdown when a TUI is present; `false` matches its test doubles.
    pub has_tui: bool,
    pub timeout_ms: Option<u64>,
    /// Text the input opens with; the cursor lands at its end.
    pub initial_value: Option<String>,
}

pub type SubmitHandler = Box<dyn FnMut(&str)>;
pub type CancelHandler = Box<dyn FnMut()>;

pub struct ExtensionInputComponent {
    theme: Theme,
    input: Input,
    base_title: String,
    remaining_seconds: Option<i64>,
    disposed: bool,
    focused: bool,
    on_submit: SubmitHandler,
    on_cancel: CancelHandler,
}

impl ExtensionInputComponent {
    pub fn new(
        theme: &Theme,
        title: &str,
        on_submit: SubmitHandler,
        on_cancel: CancelHandler,
        opts: ExtensionInputOptions,
    ) -> Self {
        let remaining_seconds = match (opts.timeout_ms, opts.has_tui) {
            (Some(timeout), true) if timeout > 0 => {
                Some(i64::try_from(timeout.div_ceil(1000)).unwrap_or(i64::MAX))
            }
            _ => None,
        };
        let mut input = Input::new(InputOptions::default());
        if let Some(initial_value) = opts.initial_value {
            // Typed in, not assigned: set_value() would leave the cursor at column 0,
            // ahead of the prefill the caller wants the user to edit.
            input.handle_input(&initial_value);
        }
        Self {
            theme: theme.clone(),
            input,
            base_title: title.to_owned(),
            remaining_seconds,
            disposed: false,
            focused: false,
            on_submit,
            on_cancel,
        }
    }

    pub fn input(&self) -> &Input {
        &self.input
    }

    pub fn input_mut(&mut self) -> &mut Input {
        &mut self.input
    }

    pub fn remaining_seconds(&self) -> Option<i64> {
        self.remaining_seconds
    }

    /// Advance the countdown by one second; expiry cancels the dialog like senpi's interval.
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
        self.theme.fg(ThemeColor::Accent, &title)
    }
}

impl Component for ExtensionInputComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = vec![dynamic_border(&self.theme, ThemeColor::Border, width), String::new()];
        lines.extend(Text::with_padding(self.title_text(), 1, 0).render(width));
        lines.push(String::new());
        lines.extend(self.input.render(width));
        lines.push(String::new());
        let hints = format!(
            "{}  {}",
            key_hint(&self.theme, "tui.select.confirm", "submit"),
            key_hint(&self.theme, "tui.select.cancel", "cancel")
        );
        lines.extend(Text::with_padding(hints, 1, 0).render(width));
        lines.push(String::new());
        lines.push(dynamic_border(&self.theme, ThemeColor::Border, width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = get_keybindings();
        if kb.matches(data, "tui.select.confirm") || data == "\n" {
            let value = self.input.get_value().to_owned();
            (self.on_submit)(&value);
        } else if kb.matches(data, "tui.select.cancel") {
            (self.on_cancel)();
        } else {
            self.input.handle_input(data);
        }
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn focusable_get(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn focusable_set(&mut self, focused: bool) {
        self.focused = focused;
        self.input.set_focused(focused);
    }
}

impl Focusable for ExtensionInputComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
        self.input.set_focused(value);
    }
}
