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
        let display_text = truncate_to_width(single_line_text, available_width, "...", false);

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

#[cfg(test)]
mod tests {
    //! Port of senpi `truncated-text.test.ts` (chalk level 3 colors written out as SGR).

    use std::sync::LazyLock;

    use regex::Regex;

    use super::*;

    static SGR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*m").unwrap());

    fn strip(line: &str) -> String {
        SGR.replace_all(line, "").into_owned()
    }

    fn red(text: &str) -> String {
        format!("\x1b[31m{text}\x1b[39m")
    }

    fn blue(text: &str) -> String {
        format!("\x1b[34m{text}\x1b[39m")
    }

    #[test]
    fn pads_output_lines_to_exactly_match_width() {
        let lines = TruncatedText::with_padding("Hello world", 1, 0).render(50);
        assert_eq!(lines.len(), 1);
        assert_eq!(visible_width(&lines[0]), 50);
    }

    #[test]
    fn pads_output_with_vertical_padding_lines_to_width() {
        let lines = TruncatedText::with_padding("Hello", 0, 2).render(40);
        assert_eq!(lines.len(), 5);
        for line in &lines {
            assert_eq!(visible_width(line), 40);
        }
    }

    #[test]
    fn truncates_long_text_and_pads_to_width() {
        let long_text =
            "This is a very long piece of text that will definitely exceed the available width";
        let lines = TruncatedText::with_padding(long_text, 1, 0).render(30);
        assert_eq!(lines.len(), 1);
        assert_eq!(visible_width(&lines[0]), 30);
        assert!(strip(&lines[0]).contains("..."));
    }

    #[test]
    fn preserves_ansi_codes_in_output_and_pads_correctly() {
        let styled = format!("{} {}", red("Hello"), blue("world"));
        let lines = TruncatedText::with_padding(styled, 1, 0).render(40);
        assert_eq!(lines.len(), 1);
        assert_eq!(visible_width(&lines[0]), 40);
        assert!(lines[0].contains("\x1b["));
    }

    #[test]
    fn truncates_styled_text_and_adds_reset_code_before_ellipsis() {
        let styled = red("This is a very long red text that will be truncated");
        let lines = TruncatedText::with_padding(styled, 1, 0).render(20);
        assert_eq!(lines.len(), 1);
        assert_eq!(visible_width(&lines[0]), 20);
        assert!(lines[0].contains("\x1b[0m..."));
    }

    #[test]
    fn handles_text_that_fits_exactly() {
        let lines = TruncatedText::with_padding("Hello world", 1, 0).render(30);
        assert_eq!(lines.len(), 1);
        assert_eq!(visible_width(&lines[0]), 30);
        assert!(!strip(&lines[0]).contains("..."));
    }

    #[test]
    fn handles_empty_text() {
        let lines = TruncatedText::with_padding("", 1, 0).render(30);
        assert_eq!(lines.len(), 1);
        assert_eq!(visible_width(&lines[0]), 30);
    }

    #[test]
    fn stops_at_newline_and_only_shows_first_line() {
        let lines =
            TruncatedText::with_padding("First line\nSecond line\nThird line", 1, 0).render(40);
        assert_eq!(lines.len(), 1);
        assert_eq!(visible_width(&lines[0]), 40);
        let stripped = strip(&lines[0]);
        let stripped = stripped.trim();
        assert!(stripped.contains("First line"));
        assert!(!stripped.contains("Second line"));
        assert!(!stripped.contains("Third line"));
    }

    #[test]
    fn truncates_first_line_even_with_newlines_in_text() {
        let text = "This is a very long first line that needs truncation\nSecond line";
        let lines = TruncatedText::with_padding(text, 1, 0).render(25);
        assert_eq!(lines.len(), 1);
        assert_eq!(visible_width(&lines[0]), 25);
        let stripped = strip(&lines[0]);
        assert!(stripped.contains("..."));
        assert!(!stripped.contains("Second line"));
    }
}
