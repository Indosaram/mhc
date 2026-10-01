//! Port of `components/user-message.ts`.
//!
//! senpi wraps the card in `AskUserAnswerChip` when the text is an answer frame; that component
//! belongs to todo 34, so this port always renders the message box.
use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::components::box_::Box as TuiBox;
use maho_tui::components::markdown::{DefaultTextStyle, MarkdownOptions, MarkdownTheme};
use maho_tui::tui::{Component, Container};
use std::sync::Arc;

use super::markdown_transform::{create_markdown_transform, MessageType, TransformedMarkdown};
use crate::theme::{Theme, ThemeBg, ThemeColor};

const OSC133_ZONE_START: &str = "\x1b]133;A\x07";
const OSC133_ZONE_END: &str = "\x1b]133;B\x07";
const OSC133_ZONE_FINAL: &str = "\x1b]133;C\x07";

pub struct UserMessageComponent {
    content: Container,
    text: String,
    theme: Theme,
    markdown_theme: MarkdownTheme,
    output_pad: usize,
    markdown_transformers: Vec<super::markdown_transform::MarkdownTransformer>,
}

impl UserMessageComponent {
    pub fn new(
        text: String,
        theme: Theme,
        markdown_theme: MarkdownTheme,
        output_pad: usize,
        markdown_transformers: Vec<super::markdown_transform::MarkdownTransformer>,
    ) -> Self {
        let mut component = Self {
            content: Container::new(),
            text,
            theme,
            markdown_theme,
            output_pad,
            markdown_transformers,
        };
        component.rebuild();
        component
    }

    pub fn set_output_pad(&mut self, padding: usize) {
        self.output_pad = padding;
        self.rebuild();
    }

    fn rebuild(&mut self) {
        self.content.clear();
        let mut content_box = TuiBox::with_padding(self.output_pad, 1);
        let bg = self.theme.clone();
        content_box.set_bg_fn(Some(Rc::new(move |text| bg.bg(ThemeBg::UserMessageBg, text))));
        let fg = self.theme.clone();
        let transform = Rc::new(create_markdown_transform(
            MessageType::User,
            false,
            self.markdown_transformers.clone(),
        ));
        content_box.add_child(Rc::new(RefCell::new(TransformedMarkdown::new(
            &self.text,
            0,
            0,
            self.markdown_theme.clone(),
            Some(DefaultTextStyle {
                color: Some(Arc::new(move |text| fg.fg(ThemeColor::UserMessageText, text))),
                ..Default::default()
            }),
            MarkdownOptions {
                preserve_ordered_list_markers: true,
                preserve_backslash_escapes: true,
                ..Default::default()
            },
            transform,
        ))));
        self.content.add_child(Rc::new(RefCell::new(content_box)));
    }
}

impl Component for UserMessageComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = self.content.render(width);
        if lines.is_empty() {
            return lines;
        }
        if lines.len() == 1 {
            lines[0] = format!("{OSC133_ZONE_START}{}{OSC133_ZONE_END}{OSC133_ZONE_FINAL}", lines[0]);
            return lines;
        }
        if let Some(first) = lines.first_mut() {
            first.insert_str(0, OSC133_ZONE_START);
        }
        if let Some(last) = lines.last_mut() {
            last.insert_str(0, &format!("{OSC133_ZONE_END}{OSC133_ZONE_FINAL}"));
        }
        lines
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
