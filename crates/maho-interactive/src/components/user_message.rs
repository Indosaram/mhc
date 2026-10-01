use std::{cell::RefCell, rc::Rc, sync::Arc};
use crate::theme::{Theme, ThemeColor, ThemeBg};
use maho_tui::{components::{box_::Box as TuiBox, markdown::{Markdown, MarkdownTheme, MarkdownOptions, DefaultTextStyle}}, tui::Component};
use super::markdown_transform::{MarkdownComponent, MarkdownTransformer, MessageType, create_markdown_transform};

pub struct UserMessageComponent {
    text: String,
    theme: Theme,
    markdown_theme: MarkdownTheme,
    output_pad: usize,
    transformers: Vec<MarkdownTransformer>,
}
impl UserMessageComponent {
    pub fn new(text: String, theme: Theme, markdown_theme: MarkdownTheme, output_pad: usize, transformers: Vec<MarkdownTransformer>) -> Self {
        Self { text, theme, markdown_theme, output_pad, transformers }
    }
    pub fn set_output_pad(&mut self, padding: usize) { self.output_pad = padding; }
}
impl Component for UserMessageComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut content = TuiBox::with_padding(self.output_pad, 1);
        let bg = self.theme.clone();
        content.set_bg_fn(Some(Rc::new(move |text| bg.bg(ThemeBg::UserMessageBg, text))));
        let fg = self.theme.clone();
        let transform = create_markdown_transform(MessageType::User, false, self.transformers.clone());
        let text = transform(&self.text, width.saturating_sub(self.output_pad * 2).max(1));
        let markdown = Markdown::new(&text, 0, 0, self.markdown_theme.clone(), Some(DefaultTextStyle {
            color: Some(Arc::new(move |text| fg.fg(ThemeColor::UserMessageText, text))), ..Default::default()
        }), MarkdownOptions { preserve_ordered_list_markers: true, preserve_backslash_escapes: true, ..Default::default() });
        content.add_child(Rc::new(RefCell::new(MarkdownComponent(markdown))));
        let mut lines = content.render(width);
        if lines.len() == 1 {
            lines[0] = format!("\x1b]133;A\x07{}\x1b]133;B\x07\x1b]133;C\x07", lines[0]);
        } else if !lines.is_empty() {
            lines[0].insert_str(0, "\x1b]133;A\x07");
            if let Some(last) = lines.last_mut() { last.insert_str(0, "\x1b]133;B\x07\x1b]133;C\x07"); }
        }
        lines
    }
}
