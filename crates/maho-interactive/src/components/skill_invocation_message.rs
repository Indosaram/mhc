use std::{cell::RefCell, rc::Rc, sync::Arc};
use crate::theme::{Theme, ThemeColor, ThemeBg};
use maho_tui::{components::{box_::Box as TuiBox, text::Text, markdown::{Markdown, MarkdownTheme, MarkdownOptions, DefaultTextStyle}}, tui::Component};
use super::markdown_transform::MarkdownComponent;

pub struct InvokedSkill { pub name: String, pub content: String }
pub struct SkillInvocationMessageComponent {
    skills: Vec<InvokedSkill>,
    expanded: bool,
    theme: Theme,
    markdown_theme: MarkdownTheme,
    expand_key: String,
}
impl SkillInvocationMessageComponent {
    pub fn new(skills: Vec<InvokedSkill>, theme: Theme, markdown_theme: MarkdownTheme, expand_key: String) -> Self {
        Self { skills, theme, markdown_theme, expand_key, expanded: false }
    }
    pub fn set_expanded(&mut self, expanded: bool) { self.expanded = expanded; }
}
impl Component for SkillInvocationMessageComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut content = TuiBox::with_padding(1, 1);
        let theme = self.theme.clone();
        content.set_bg_fn(Some(Rc::new(move |text| theme.bg(ThemeBg::CustomMessageBg, text))));
        if self.expanded {
            content.add_child(Rc::new(RefCell::new(Text::with_padding(self.theme.fg(ThemeColor::CustomMessageLabel, "\x1b[1m[skill]\x1b[22m"), 0, 0))));
            let body = self.skills.iter().map(|skill| format!("**{}**\n\n{}", skill.name, skill.content)).collect::<Vec<_>>().join("\n\n");
            let theme = self.theme.clone();
            let style = DefaultTextStyle { color: Some(Arc::new(move |text| theme.fg(ThemeColor::CustomMessageText, text))), ..Default::default() };
            content.add_child(Rc::new(RefCell::new(MarkdownComponent(Markdown::new(&body, 0, 0, self.markdown_theme.clone(), Some(style), MarkdownOptions::default())))));
        } else {
            let names = self.skills.iter().map(|skill| skill.name.as_str()).collect::<Vec<_>>().join(", ");
            let text = self.theme.fg(ThemeColor::CustomMessageLabel, "\x1b[1m[skill]\x1b[22m ") + &self.theme.fg(ThemeColor::CustomMessageText, &names) + &self.theme.fg(ThemeColor::Dim, &format!(" ({} to expand)", self.expand_key));
            content.add_child(Rc::new(RefCell::new(Text::with_padding(text, 0, 0))));
        }
        content.render(width)
    }
}
