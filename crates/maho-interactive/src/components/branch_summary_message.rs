use crate::theme::{Theme, ThemeColor, ThemeBg};
use maho_tui::{components::{box_::Box as TuiBox, text::Text, markdown::{Markdown, MarkdownTheme, MarkdownOptions, DefaultTextStyle}}, tui::Component};
use std::sync::Arc;

pub struct BranchSummaryMessageComponent {
    summary: String,
    expanded: bool,
    theme: Theme,
    markdown_theme: MarkdownTheme,
    expand_key: String,
    markdown: Option<std::rc::Rc<std::cell::RefCell<super::markdown_transform::MarkdownComponent>>>,
}
impl BranchSummaryMessageComponent {
    pub fn new(summary: String, theme: Theme, markdown_theme: MarkdownTheme, expand_key: String) -> Self {
        Self { summary, theme, markdown_theme, expand_key, expanded: false, markdown: None }
    }
    pub fn set_expanded(&mut self, expanded: bool) { self.expanded = expanded; self.markdown = None; }
}
impl Component for BranchSummaryMessageComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut content = TuiBox::with_padding(1, 1);
        let theme = self.theme.clone();
        content.set_bg_fn(Some(std::rc::Rc::new(move |text| theme.bg(ThemeBg::CustomMessageBg, text))));
        content.add_child(std::rc::Rc::new(std::cell::RefCell::new(Text::with_padding(self.theme.fg(ThemeColor::CustomMessageLabel, "\x1b[1m[branch]\x1b[22m"), 0, 0))));
        content.add_child(std::rc::Rc::new(std::cell::RefCell::new(maho_tui::components::spacer::Spacer::new(1))));
        if self.expanded {
            // Kept across frames: a rebuilt Markdown gets a fresh style id and misses the render cache.
            let markdown = self.markdown.get_or_insert_with(|| {
                let theme = self.theme.clone();
                let style = DefaultTextStyle { color: Some(Arc::new(move |text| theme.fg(ThemeColor::CustomMessageText, text))), ..Default::default() };
                let markdown = Markdown::new(&format!("**Branch Summary**\n\n{}", self.summary), 0, 0, self.markdown_theme.clone(), Some(style), MarkdownOptions::default());
                std::rc::Rc::new(std::cell::RefCell::new(super::markdown_transform::MarkdownComponent(markdown)))
            });
            content.add_child(markdown.clone());
        } else {
            let text = self.theme.fg(ThemeColor::CustomMessageText, "Branch summary (") + &self.theme.fg(ThemeColor::Dim, &self.expand_key) + &self.theme.fg(ThemeColor::CustomMessageText, " to expand)");
            content.add_child(std::rc::Rc::new(std::cell::RefCell::new(Text::with_padding(text, 0, 0))));
        }
        content.render(width)
    }
    fn invalidate(&mut self) { self.markdown = None; }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ColorMode;

    #[test]
    fn expanded_markdown_is_reused_across_frames_and_rebuilt_on_toggle() {
        let theme = Theme::builtin("dark", ColorMode::Color256).expect("builtin theme");
        let markdown_theme = super::super::markdown_transform::get_markdown_theme(&theme);
        let mut component = BranchSummaryMessageComponent::new("Retained **context**.".to_owned(), theme, markdown_theme, String::new());
        component.set_expanded(true);
        let first = component.render(40);
        let cached = component.markdown.clone().expect("markdown kept after render");
        assert_eq!(component.render(40), first);
        assert!(std::rc::Rc::ptr_eq(&cached, component.markdown.as_ref().expect("still kept")));
        component.set_expanded(false);
        assert!(component.markdown.is_none());
    }
}
