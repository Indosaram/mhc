//! Port of senpi `packages/tui/src/components/text.ts`.

use crate::components::box_::BackgroundFn;
use crate::tui::Component;
use crate::utils::{apply_background_to_line, visible_width, wrap_text_with_ansi};

pub struct Text {
    text: String,
    padding_x: usize,
    padding_y: usize,
    custom_bg_fn: Option<BackgroundFn>,
    cached_text: Option<String>,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
}

impl Text {
    pub fn new(text: impl Into<String>) -> Self {
        Self::with_padding(text, 1, 1)
    }

    pub fn with_padding(text: impl Into<String>, padding_x: usize, padding_y: usize) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
            custom_bg_fn: None,
            cached_text: None,
            cached_width: None,
            cached_lines: None,
        }
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cached_text = None;
        self.cached_width = None;
        self.cached_lines = None;
    }

    pub fn set_custom_bg_fn(&mut self, custom_bg_fn: Option<BackgroundFn>) {
        self.custom_bg_fn = custom_bg_fn;
        self.cached_text = None;
        self.cached_width = None;
        self.cached_lines = None;
    }
}

impl Component for Text {
    fn invalidate(&mut self) {
        self.cached_text = None;
        self.cached_width = None;
        self.cached_lines = None;
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        if let (Some(lines), Some(cached_text), Some(cached_width)) =
            (&self.cached_lines, &self.cached_text, self.cached_width)
            && cached_text == &self.text
            && cached_width == width
        {
            return lines.clone();
        }

        if self.text.trim().is_empty() {
            self.cached_text = Some(self.text.clone());
            self.cached_width = Some(width);
            self.cached_lines = Some(Vec::new());
            return Vec::new();
        }

        let normalized_text = self.text.replace('\t', "   ");
        let padding_x = self.padding_x.min(width.saturating_sub(1) / 2);
        let content_width = (width.saturating_sub(padding_x * 2)).max(1);

        let wrapped_lines = wrap_text_with_ansi(&normalized_text, content_width);

        let left_margin = " ".repeat(padding_x);
        let right_margin = " ".repeat(padding_x);
        let mut content_lines = Vec::with_capacity(wrapped_lines.len());
        for line in &wrapped_lines {
            let line_with_margins = format!("{left_margin}{line}{right_margin}");
            let rendered = match &self.custom_bg_fn {
                Some(bg_fn) => apply_background_to_line(&line_with_margins, width, bg_fn.as_ref()),
                None => {
                    let visible_len = visible_width(&line_with_margins);
                    let padding_needed = width.saturating_sub(visible_len);
                    format!("{line_with_margins}{}", " ".repeat(padding_needed))
                }
            };
            content_lines.push(rendered);
        }

        let empty_line = " ".repeat(width);
        let mut empty_lines = Vec::with_capacity(self.padding_y);
        for _ in 0..self.padding_y {
            let line = match &self.custom_bg_fn {
                Some(bg_fn) => apply_background_to_line(&empty_line, width, bg_fn.as_ref()),
                None => empty_line.clone(),
            };
            empty_lines.push(line);
        }

        let mut result = Vec::with_capacity(empty_lines.len() * 2 + content_lines.len());
        result.extend(empty_lines.iter().cloned());
        result.extend(content_lines);
        result.extend(empty_lines);

        self.cached_text = Some(self.text.clone());
        self.cached_width = Some(width);
        self.cached_lines = Some(result.clone());

        if result.is_empty() {
            vec![String::new()]
        } else {
            result
        }
    }
}
