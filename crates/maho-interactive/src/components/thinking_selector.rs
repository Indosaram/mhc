use std::{cell::RefCell, rc::Rc, sync::Arc};
use maho_ai::types::ModelThinkingLevel;
use maho_tui::{components::{input::{Input, InputOptions}, select_list::{SelectItem, SelectList}, text::Text}, fuzzy::fuzzy_filter, keybindings::KeybindingsManager, tui::{Component, Focusable}};
use crate::theme::{Theme, ThemeColor};
use super::{keybinding_hints::key_display_text, theme_selector::{dynamic_border, select_list_theme, theme_select_list_layout}};

type LevelCallback = Rc<RefCell<Box<dyn FnMut(ModelThinkingLevel)>>>;

pub struct ThinkingSelectorOptions {
    pub current: ModelThinkingLevel,
    pub available: Vec<ModelThinkingLevel>,
    pub default: Option<ModelThinkingLevel>,
    pub on_select: Box<dyn FnMut(ModelThinkingLevel)>,
    pub on_cancel: Box<dyn FnMut()>,
    pub on_select_as_default: Option<Box<dyn FnMut(ModelThinkingLevel)>>,
}

pub struct ThinkingSelectorComponent {
    theme: Theme,
    keybindings: Arc<KeybindingsManager>,
    search_input: Input,
    select_list: SelectList,
    all_items: Vec<SelectItem>,
    on_select: LevelCallback,
    on_cancel: Rc<RefCell<Box<dyn FnMut()>>>,
    on_default: Option<Box<dyn FnMut(ModelThinkingLevel)>>,
}

fn description(level: ModelThinkingLevel) -> &'static str {
    match level {
        ModelThinkingLevel::Off => "No reasoning",
        ModelThinkingLevel::Minimal => "Very brief reasoning (~1k tokens)",
        ModelThinkingLevel::Low => "Light reasoning (~2k tokens)",
        ModelThinkingLevel::Medium => "Moderate reasoning (~8k tokens)",
        ModelThinkingLevel::High => "Deep reasoning (~16k tokens)",
        ModelThinkingLevel::Xhigh => "Extended reasoning (~32k tokens or native xhigh effort)",
        ModelThinkingLevel::Max => "Maximum reasoning",
    }
}

impl ThinkingSelectorComponent {
    pub fn new(theme: &Theme, keybindings: Arc<KeybindingsManager>, options: ThinkingSelectorOptions) -> Self {
        let items: Vec<_> = options.available.iter().map(|level| SelectItem {
            value: level.as_str().into(),
            label: format!("{}{}", if *level == options.current { "✓ " } else { "  " }, level.as_str()),
            description: Some(format!("{}{}", description(*level), if Some(*level) == options.default { " · default" } else { "" })),
        }).collect();
        let mut result = Self {
            theme: theme.clone(), keybindings, search_input: Input::new(InputOptions::default()),
            select_list: SelectList::new(Vec::new(), 1, select_list_theme(theme), theme_select_list_layout()),
            all_items: items.clone(), on_select: Rc::new(RefCell::new(options.on_select)),
            on_cancel: Rc::new(RefCell::new(options.on_cancel)), on_default: options.on_select_as_default,
        };
        result.select_list = result.build_list(items, Some(options.current.as_str()));
        result
    }
    fn build_list(&self, items: Vec<SelectItem>, preselect: Option<&str>) -> SelectList {
        let index = items.iter().position(|item| Some(item.value.as_str()) == preselect);
        let mut list = SelectList::new(items.clone(), items.len().max(1), select_list_theme(&self.theme), theme_select_list_layout());
        if let Some(index) = index { list.set_selected_index(index); }
        let select = self.on_select.clone();
        list.on_select = Some(Box::new(move |item| { if let Some(level) = ModelThinkingLevel::parse(&item.value) { (select.borrow_mut())(level); } }));
        let cancel = self.on_cancel.clone();
        list.on_cancel = Some(Box::new(move || (cancel.borrow_mut())()));
        list
    }
    pub fn select_list(&self) -> &SelectList { &self.select_list }
}

impl Component for ThinkingSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = vec![dynamic_border(&self.theme, ThemeColor::Border, width), String::new()];
        lines.extend(Text::with_padding("Thinking Level", 0, 0).render(width));
        lines.push(String::new());
        lines.extend(Text::with_padding(format!("{} cycles thinking levels in-session", key_display_text("app.thinking.cycle")), 0, 0).render(width));
        lines.push(String::new());
        lines.extend(self.search_input.render(width));
        lines.push(String::new());
        lines.extend(self.select_list.render(width));
        lines.push(String::new());
        let hint = format!("  {} to select · {} to set as default · {} to cancel", key_display_text("tui.select.confirm"), key_display_text("app.thinking.save"), key_display_text("tui.select.cancel"));
        lines.extend(Text::with_padding(self.theme.fg(ThemeColor::Dim, &hint), 0, 0).render(width));
        lines.push(dynamic_border(&self.theme, ThemeColor::Border, width));
        lines
    }
    fn handle_input(&mut self, data: &str) {
        if self.keybindings.matches(data, "app.thinking.save") && self.on_default.is_some() {
            if let Some(item) = self.select_list.get_selected_item() && let Some(level) = ModelThinkingLevel::parse(&item.value) && let Some(callback) = &mut self.on_default { callback(level); }
            return;
        }
        if ["tui.select.up", "tui.select.down", "tui.select.confirm", "tui.select.cancel"].iter().any(|id| self.keybindings.matches(data, id)) {
            self.select_list.handle_input(data);
        } else {
            self.search_input.handle_input(data);
            let query = self.search_input.get_value();
            let items = if query.is_empty() { self.all_items.clone() } else { fuzzy_filter(&self.all_items, query, |item| format!("{} {}", item.value, item.description.as_deref().unwrap_or_default())) };
            let selected = self.select_list.get_selected_item().map(|item| item.value.clone());
            self.select_list = self.build_list(items, selected.as_deref());
        }
    }
    fn has_input_handler(&self) -> bool { true }
}

impl Focusable for ThinkingSelectorComponent {
    fn focused(&self) -> bool { self.search_input.focused() }
    fn set_focused(&mut self, value: bool) { self.search_input.set_focused(value); }
}
