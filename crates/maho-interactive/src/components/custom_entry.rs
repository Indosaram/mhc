use std::{cell::RefCell, rc::Rc};
use serde_json::Value;
use crate::theme::{Theme, ThemeColor, ThemeBg};
use maho_tui::{components::{box_::Box as TuiBox, spacer::Spacer, text::Text}, tui::{Component, Container}};

pub type EntryRenderer = Rc<dyn Fn(&Value, bool, &Theme) -> Result<Option<Rc<RefCell<dyn Component>>>, String>>;
pub type Replaces = Rc<dyn Fn(&Value, &Value) -> bool>;
pub struct CustomEntryComponent {
    pub custom_entry: Value,
    renderer: EntryRenderer,
    expanded: bool,
    theme: Theme,
    content: Container,
    has_content: bool,
}
impl CustomEntryComponent {
    pub fn new(entry: Value, renderer: EntryRenderer, theme: Theme) -> Self {
        let mut component = Self { custom_entry: entry, renderer, theme, expanded: false, content: Container::new(), has_content: false };
        component.rebuild();
        component
    }
    pub fn has_content(&self) -> bool { self.has_content }
    pub fn set_expanded(&mut self, expanded: bool) {
        if self.expanded != expanded { self.expanded = expanded; self.rebuild(); }
    }
    fn rebuild(&mut self) {
        self.content.clear();
        let component = match (self.renderer)(&self.custom_entry, self.expanded, &self.theme) {
            Ok(component) => component,
            Err(error) => {
                let mut content = TuiBox::with_padding(1, 1);
                let theme = self.theme.clone();
                content.set_bg_fn(Some(Rc::new(move |text| theme.bg(ThemeBg::CustomMessageBg, text))));
                let label = format!("[{}] renderer failed: {error}", self.custom_entry["customType"].as_str().unwrap_or_default());
                content.add_child(Rc::new(RefCell::new(Text::with_padding(self.theme.fg(ThemeColor::Error, &label), 0, 0))));
                Some(Rc::new(RefCell::new(content)) as Rc<RefCell<dyn Component>>)
            }
        };
        self.has_content = component.is_some();
        if let Some(component) = component {
            self.content.add_child(Rc::new(RefCell::new(Spacer::new(1))));
            self.content.add_child(component);
        }
    }
}
pub enum EntryCard<'a> { Entry(&'a CustomEntryComponent), Transient, Other }
pub fn replaced_entry_card_index(children: &[EntryCard<'_>], insert_index: usize, entry: &Value, replaces: Option<&Replaces>) -> Option<usize> {
    let replaces = replaces?;
    let mut index = insert_index.checked_sub(1)?;
    while matches!(children.get(index), Some(EntryCard::Transient)) { index = index.checked_sub(1)?; }
    let Some(EntryCard::Entry(previous)) = children.get(index) else { return None; };
    (previous.custom_entry["customType"] == entry["customType"] && replaces(&previous.custom_entry, entry)).then_some(index)
}
impl Component for CustomEntryComponent {
    fn render(&mut self, width: usize) -> Vec<String> { self.content.render(width) }
    fn invalidate(&mut self) { self.content.invalidate(); self.rebuild(); }
    fn dispose(&mut self) { self.content.dispose(); }
    fn handle_mouse(&mut self, event: &maho_tui::tui::TuiMouseEvent) -> Option<maho_tui::tui::TuiMouseEventResult> { self.content.handle_mouse(event) }
}
