use maho_tui::{components::select_list::{SelectItem, SelectList}, tui::Component};
use crate::theme::{Theme, ThemeColor};
use super::theme_selector::{dynamic_border, select_list_theme, theme_select_list_layout};

pub struct ShowImagesSelectorComponent { theme: Theme, select_list: SelectList }

impl ShowImagesSelectorComponent {
    pub fn new(theme: &Theme, current: bool, mut on_select: Box<dyn FnMut(bool)>, on_cancel: Box<dyn FnMut()>) -> Self {
        let items = vec![
            SelectItem { value: "yes".into(), label: "Yes".into(), description: Some("Show images inline in terminal".into()) },
            SelectItem { value: "no".into(), label: "No".into(), description: Some("Show text placeholder instead".into()) },
        ];
        let mut select_list = SelectList::new(items, 5, select_list_theme(theme), theme_select_list_layout());
        select_list.set_selected_index(usize::from(!current));
        select_list.on_select = Some(Box::new(move |item| on_select(item.value == "yes")));
        select_list.on_cancel = Some(on_cancel);
        Self { theme: theme.clone(), select_list }
    }
    pub fn select_list(&self) -> &SelectList { &self.select_list }
    pub fn select_list_mut(&mut self) -> &mut SelectList { &mut self.select_list }
}

impl Component for ShowImagesSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = vec![dynamic_border(&self.theme, ThemeColor::Border, width)];
        lines.extend(self.select_list.render(width));
        lines.push(dynamic_border(&self.theme, ThemeColor::Border, width));
        lines
    }
    fn handle_input(&mut self, data: &str) { self.select_list.handle_input(data); }
    fn has_input_handler(&self) -> bool { true }
    fn invalidate(&mut self) { self.select_list.invalidate(); }
}
