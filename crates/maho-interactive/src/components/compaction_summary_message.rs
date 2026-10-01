use std::{cell::RefCell, rc::Rc, sync::Arc};
use serde_json::Value;
use crate::theme::{Theme, ThemeColor, ThemeBg};
use maho_tui::{components::{box_::Box as TuiBox, spacer::Spacer, text::Text, markdown::{Markdown, MarkdownTheme, MarkdownOptions, DefaultTextStyle}}, tui::Component};
use super::markdown_transform::MarkdownComponent;

pub fn format_count(count: u64) -> String {
    let digits = count.to_string();
    let mut result = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) { result.push(','); }
        result.push(digit);
    }
    result
}
pub fn sanitize_compaction_summary(summary: &str) -> String {
    maho_tui::utils::strip_terminal_sequences(summary).replace("\r\n", "\n").replace('\r', "\n")
        .chars().filter(|c| *c == '\n' || !c.is_control()).collect()
}
pub fn format_compaction_details(details: &Value) -> Option<String> {
    if details["schema"] != "senpi.compaction.openai-remote.v1" { return None; }
    let retained = details["retainedInputItemCount"].as_u64().map_or_else(|| "native replay active".to_owned(), |n| format!("{} retained items", format_count(n)));
    let requested = details["requestInputItemCount"].as_u64().map_or_else(String::new, |n| format!(" from {} OpenAI input items", format_count(n)));
    let route = if details["transport"] == "websocket" { "OpenAI Responses WebSocket compaction" } else { "OpenAI remote compact API" };
    Some(format!("{route}: {retained}{requested}"))
}
pub struct CompactionSummaryMessageComponent {
    message: Value,
    expanded: bool,
    theme: Theme,
    markdown_theme: MarkdownTheme,
    expand_key: String,
}
impl CompactionSummaryMessageComponent {
    pub fn new(message: Value, theme: Theme, markdown_theme: MarkdownTheme, expand_key: String) -> Self {
        Self { message, theme, markdown_theme, expand_key, expanded: false }
    }
    pub fn set_expanded(&mut self, expanded: bool) { self.expanded = expanded; }
}
impl Component for CompactionSummaryMessageComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut content = TuiBox::with_padding(1, 1);
        let theme = self.theme.clone();
        content.set_bg_fn(Some(Rc::new(move |text| theme.bg(ThemeBg::CustomMessageBg, text))));
        content.add_child(Rc::new(RefCell::new(Text::with_padding(self.theme.fg(ThemeColor::CustomMessageLabel, "\x1b[1m[compaction]\x1b[22m"), 0, 0))));
        content.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        let tokens = format_count(self.message["tokensBefore"].as_u64().unwrap_or_default());
        let details = format_compaction_details(&self.message["details"]);
        if self.expanded {
            let theme = self.theme.clone();
            let style = DefaultTextStyle { color: Some(Arc::new(move |text| theme.fg(ThemeColor::CustomMessageText, text))), ..Default::default() };
            let detail_line = details.map_or_else(|| "\n\n".to_owned(), |details| format!("\n{details}\n\n"));
            let body = format!("**Compacted from {tokens} tokens**{detail_line}{}", sanitize_compaction_summary(self.message["summary"].as_str().unwrap_or_default()));
            content.add_child(Rc::new(RefCell::new(MarkdownComponent(Markdown::new(&body, 0, 0, self.markdown_theme.clone(), Some(style), MarkdownOptions::default())))));
        } else {
            let prefix = details.map_or_else(String::new, |details| format!("{details}; "));
            let text = self.theme.fg(ThemeColor::CustomMessageText, &format!("{prefix}compacted from {tokens} tokens (")) + &self.theme.fg(ThemeColor::Dim, &self.expand_key) + &self.theme.fg(ThemeColor::CustomMessageText, " to expand)");
            content.add_child(Rc::new(RefCell::new(Text::with_padding(text, 0, 0))));
        }
        content.render(width)
    }
}
