use std::{cell::RefCell, rc::Rc, sync::Arc};
use serde_json::Value;
use crate::theme::{Theme, ThemeColor, ThemeBg};
use maho_tui::{components::{box_::Box as TuiBox, spacer::Spacer, text::Text, markdown::{Markdown, MarkdownTheme, MarkdownOptions, DefaultTextStyle}}, tui::{Component, Container}};
use super::markdown_transform::MarkdownComponent;

pub type MessageRenderer = Rc<dyn Fn(&Value, bool, usize, &Theme) -> Result<Option<Rc<RefCell<dyn Component>>>, String>>;
pub struct CustomMessageComponent {
    message: Value,
    renderer: Option<MessageRenderer>,
    expanded: bool,
    output_pad: usize,
    theme: Theme,
    markdown_theme: MarkdownTheme,
    content: Container,
}
impl CustomMessageComponent {
    pub fn new(message: Value, renderer: Option<MessageRenderer>, theme: Theme, markdown_theme: MarkdownTheme, output_pad: usize) -> Self {
        let mut component = Self { message, renderer, theme, markdown_theme, output_pad, expanded: false, content: Container::new() };
        component.rebuild();
        component
    }
    pub fn set_expanded(&mut self, expanded: bool) {
        if self.expanded != expanded { self.expanded = expanded; self.rebuild(); }
    }
    pub fn set_output_pad(&mut self, padding: usize) {
        if self.output_pad != padding { self.output_pad = padding; self.rebuild(); }
    }
    fn rebuild(&mut self) {
        self.content.clear();
        self.content.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        if let Some(renderer) = &self.renderer
            && let Ok(Some(component)) = renderer(&self.message, self.expanded, self.output_pad, &self.theme) {
            self.content.add_child(component);
            return;
        }
        let mut content = TuiBox::with_padding(1, 1);
        let theme = self.theme.clone();
        content.set_bg_fn(Some(Rc::new(move |text| theme.bg(ThemeBg::CustomMessageBg, text))));
        let label = self.theme.fg(ThemeColor::CustomMessageLabel, &format!("\x1b[1m[{}]\x1b[22m", self.message["customType"].as_str().unwrap_or_default()));
        content.add_child(Rc::new(RefCell::new(Text::with_padding(label, 0, 0))));
        content.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        let text = self.message["content"].as_str().map(str::to_owned).unwrap_or_else(|| {
            self.message["content"].as_array().into_iter().flatten().filter(|c| c["type"] == "text")
                .filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n")
        });
        let theme = self.theme.clone();
        let style = DefaultTextStyle { color: Some(Arc::new(move |text| theme.fg(ThemeColor::CustomMessageText, text))), ..Default::default() };
        content.add_child(Rc::new(RefCell::new(MarkdownComponent(Markdown::new(&text, 0, 0, self.markdown_theme.clone(), Some(style), MarkdownOptions::default())))));
        self.content.add_child(Rc::new(RefCell::new(content)));
    }
}
impl Component for CustomMessageComponent {
    fn render(&mut self, width: usize) -> Vec<String> { self.content.render(width) }
    fn invalidate(&mut self) { self.content.invalidate(); self.rebuild(); }
    fn dispose(&mut self) { self.content.dispose(); }
    fn handle_mouse(&mut self, event: &maho_tui::tui::TuiMouseEvent) -> Option<maho_tui::tui::TuiMouseEventResult> { self.content.handle_mouse(event) }
}
