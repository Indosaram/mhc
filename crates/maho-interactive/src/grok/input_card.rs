//! Port of senpi `packages/coding-agent/src/modes/interactive/grok/input-card.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::tui::Component;
use maho_tui::utils::{truncate_to_width, visible_width};

use crate::theme::Theme;

use super::chrome_tokens::get_grok_chrome_tokens;

pub struct GrokInputCard {
    editor: Rc<RefCell<dyn Component>>,
    theme: Theme,
}

impl GrokInputCard {
    pub fn new(editor: Rc<RefCell<dyn Component>>, theme: Theme) -> Self {
        Self { editor, theme }
    }
}

impl Component for GrokInputCard {
    fn render(&mut self, width: usize) -> Vec<String> {
        if width < 3 {
            return self.editor.borrow_mut().render(width);
        }

        let tokens = get_grok_chrome_tokens(&self.theme);
        let content_width = width - 2;
        let horizontal = "─".repeat(content_width);
        let editor_lines = self.editor.borrow_mut().render(content_width);
        let content = if editor_lines.is_empty() { vec![String::new()] } else { editor_lines };
        let padded_lines: Vec<String> = content
            .into_iter()
            .map(|line| {
                let truncated = truncate_to_width(&line, content_width, "", false);
                let padding = content_width.saturating_sub(visible_width(&truncated));
                format!("{truncated}{}", " ".repeat(padding))
            })
            .collect();

        let mut lines = vec![(tokens.input_border)(&format!("╭{horizontal}╮"))];
        lines.extend(padded_lines.into_iter().map(|line| {
            format!(
                "{}{}{}",
                (tokens.input_border)("│"),
                (tokens.input_interior)(&line),
                (tokens.input_border)("│")
            )
        }));
        lines.push((tokens.input_border)(&format!("╰{horizontal}╯")));
        lines
    }

    fn invalidate(&mut self) {
        self.editor.borrow_mut().invalidate();
    }

    fn dispose(&mut self) {
        self.editor.borrow_mut().dispose();
    }
}
