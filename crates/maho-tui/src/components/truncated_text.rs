//! Port of senpi `packages/tui/src/components/truncated-text.ts`.

use crate::tui::Component;
use crate::utils::{truncate_to_width, visible_width};

pub struct TruncatedText {
    text: String,
    padding_x: usize,
    padding_y: usize,
}

impl TruncatedText {
    pub fn new(text: impl Into<String>) -> Self {
        Self::with_padding(text, 0, 0)
    }

    pub fn with_padding(text: impl Into<String>, padding_x: usize, padding_y: usize) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
        }
    }
}

impl Component for TruncatedText {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut result = Vec::new();
        let empty_line = " ".repeat(width);
        for _ in 0..self.padding_y {
            result.push(empty_line.clone());
        }

        let available_width = width.saturating_sub(self.padding_x * 2).max(1);
        let single_line_text = self.text.split('\n').next().unwrap_or("");
        let display_text = truncate_to_width(single_line_text, available_width, "\u{2026}", false);

        let left_padding = " ".repeat(self.padding_x);
        let right_padding = " ".repeat(self.padding_x);
        let line_with_padding = format!("{left_padding}{display_text}{right_padding}");

        let line_visible_width = visible_width(&line_with_padding);
        let padding_needed = width.saturating_sub(line_visible_width);
        let final_line = format!("{line_with_padding}{}", " ".repeat(padding_needed));

        result.push(final_line);
        for _ in 0..self.padding_y {
            result.push(empty_line.clone());
        }
        result
    }
}
