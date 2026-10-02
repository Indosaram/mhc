use std::sync::Arc;
use maho_ai::types::Model;
use maho_tui::{components::{input::{Input, InputOptions}, text::Text}, fuzzy::fuzzy_filter, keybindings::KeybindingsManager, keys::matches_key, tui::{Component, Focusable}};
use crate::{model_search::{ModelSearchItem, get_model_search_text}, theme::{Theme, ThemeColor}};
use super::{favorite_model_ids::{clear_favorite_models, get_sorted_favorite_model_ids, is_favorite_model, move_favorite_model}, keybinding_hints::key_display_text, theme_selector::dynamic_border};

pub type EnabledIds = Option<Vec<String>>;
pub type ModelsChange = Box<dyn FnMut(EnabledIds)>;
pub struct ModelsConfig { pub all_models: Vec<Model>, pub enabled_model_ids: EnabledIds, pub refresh_status: Option<String> }
pub struct ModelsCallbacks { pub on_change: ModelsChange, pub on_persist: ModelsChange, pub on_cancel: Box<dyn FnMut()> }

pub struct ScopedModelsSelectorComponent {
    theme: Theme,
    keybindings: Arc<KeybindingsManager>,
    models: Vec<Model>,
    all_ids: Vec<String>,
    enabled_ids: EnabledIds,
    filtered: Vec<String>,
    selected_index: usize,
    search_input: Input,
    callbacks: ModelsCallbacks,
    dirty: bool,
    refresh_status: Option<(String, ThemeColor)>,
}

impl ScopedModelsSelectorComponent {
    pub fn new(theme: &Theme, keybindings: Arc<KeybindingsManager>, config: ModelsConfig, callbacks: ModelsCallbacks) -> Self {
        let all_ids: Vec<_> = config.all_models.iter().map(|model| format!("{}/{}", model.provider, model.id)).collect();
        let filtered = get_sorted_favorite_model_ids(&config.enabled_model_ids, &all_ids);
        Self { theme: theme.clone(), keybindings, models: config.all_models, all_ids, enabled_ids: config.enabled_model_ids, filtered, selected_index: 0, search_input: Input::new(InputOptions::default()), callbacks, dirty: false, refresh_status: config.refresh_status.map(|text| (text, ThemeColor::Muted)) }
    }
    fn model(&self, id: &str) -> Option<&Model> { self.models.iter().find(|model| format!("{}/{}", model.provider, model.id) == id) }
    fn refresh(&mut self) {
        let items = get_sorted_favorite_model_ids(&self.enabled_ids, &self.all_ids);
        let query = self.search_input.get_value();
        self.filtered = if query.is_empty() { items } else { fuzzy_filter(&items, query, |id| self.model(id).map_or_else(|| id.clone(), |model| get_model_search_text(&ModelSearchItem { id: model.id.clone(), provider: model.provider.clone(), name: Some(model.name.clone()) }))) };
        self.selected_index = self.selected_index.min(self.filtered.len().saturating_sub(1));
    }
    pub fn update_models(&mut self, models: &[Model], enabled: Option<EnabledIds>) {
        let selected = self.filtered.get(self.selected_index).cloned();
        if let Some(enabled) = enabled { self.enabled_ids = enabled; }
        self.models = models.to_vec();
        self.all_ids = models.iter().map(|model| format!("{}/{}", model.provider, model.id)).collect();
        self.refresh();
        if let Some(index) = selected.and_then(|id| self.filtered.iter().position(|candidate| candidate == &id)) { self.selected_index = index; }
    }
    pub fn set_refresh_status(&mut self, message: &str, color: ThemeColor) {
        if let Some(status) = &mut self.refresh_status { *status = (message.into(), color); }
    }
    pub fn search_input_mut(&mut self) -> &mut Input { &mut self.search_input }
    pub fn enabled_ids(&self) -> &EnabledIds { &self.enabled_ids }
    fn enable(&mut self, targets: &[String]) {
        if let Some(ids) = &mut self.enabled_ids { for id in targets { if !ids.contains(id) { ids.push(id.clone()); } } }
    }
    fn changed(&mut self) { self.dirty = true; self.refresh(); (self.callbacks.on_change)(self.enabled_ids.clone()); }
    fn footer(&self) -> String {
        let count = match &self.enabled_ids {
            None => "all enabled".into(),
            Some(ids) => {
                let enabled = ids.iter().filter(|id| self.model(id).is_some()).count();
                let unavailable = ids.len() - enabled;
                format!("{enabled}/{} enabled{}", self.all_ids.len(), if unavailable > 0 { format!(" · {unavailable} unavailable") } else { String::new() })
            }
        };
        let parts = [format!("{} toggle", key_display_text("tui.select.confirm")), format!("{} all", key_display_text("app.models.enableAll")), format!("{} clear", key_display_text("app.models.clearAll")), format!("{} provider", key_display_text("app.models.toggleProvider")), format!("{}/{} reorder", key_display_text("app.models.reorderUp"), key_display_text("app.models.reorderDown")), format!("{} save", key_display_text("app.models.save")), count];
        let text = format!("  {}{}", parts.join(" · "), if self.dirty { " " } else { "" });
        self.theme.fg(ThemeColor::Dim, &text) + &if self.dirty { self.theme.fg(ThemeColor::Warning, "(unsaved)") } else { String::new() }
    }
}

impl Component for ScopedModelsSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = vec![dynamic_border(&self.theme, ThemeColor::Border, width), String::new()];
        lines.extend(Text::with_padding(self.theme.fg(ThemeColor::Accent, &self.theme.bold("Model Configuration")), 0, 0).render(width));
        lines.extend(Text::with_padding(self.theme.fg(ThemeColor::Muted, &format!("Session-only. {} to save to settings.", key_display_text("app.models.save"))), 0, 0).render(width));
        lines.push(String::new()); lines.extend(self.search_input.render(width)); lines.push(String::new());
        if self.filtered.is_empty() { lines.extend(Text::with_padding(self.theme.fg(ThemeColor::Muted, "  No matching models"), 0, 0).render(width)); }
        else {
            let start = self.selected_index.saturating_sub(4).min(self.filtered.len().saturating_sub(8));
            let end = (start + 8).min(self.filtered.len());
            for (index, id) in self.filtered.iter().enumerate().take(end).skip(start) {
                let model = self.model(id);
                let selected = index == self.selected_index;
                let prefix = if selected { self.theme.fg(ThemeColor::Accent, "→ ") } else { "  ".into() };
                let text = model.map_or_else(|| self.theme.strikethrough(id), |model| model.id.clone());
                let text = if selected { self.theme.fg(ThemeColor::Accent, &text) } else { text };
                let badge = self.theme.fg(ThemeColor::Muted, &model.map_or_else(|| " [unavailable]".into(), |model| format!(" [{}]", model.provider)));
                let status = if model.is_some() && is_favorite_model(&self.enabled_ids, id) { self.theme.fg(ThemeColor::Accent, "✓ ") } else { "  ".into() };
                lines.extend(Text::with_padding(format!("{prefix}{status}{text}{badge}"), 0, 0).render(width));
            }
            if start > 0 || end < self.filtered.len() { lines.extend(Text::with_padding(self.theme.fg(ThemeColor::Muted, &format!("  ({}/{})", self.selected_index + 1, self.filtered.len())), 0, 0).render(width)); }
            if let Some(id) = self.filtered.get(self.selected_index) {
                lines.push(String::new());
                let label = self.model(id).map_or_else(|| "Model unavailable".into(), |model| format!("Model Name: {}", model.name));
                lines.extend(Text::with_padding(self.theme.fg(ThemeColor::Muted, &format!("  {label}")), 0, 0).render(width));
            }
        }
        lines.push(String::new());
        if let Some((text, color)) = &self.refresh_status { lines.extend(Text::with_padding(self.theme.fg(*color, &format!("  {text}")), 0, 0).render(width)); }
        lines.extend(Text::with_padding(self.footer(), 0, 0).render(width));
        lines.push(dynamic_border(&self.theme, ThemeColor::Border, width)); lines
    }
    fn handle_input(&mut self, data: &str) {
        let kb = &self.keybindings;
        if kb.matches(data, "tui.select.up") || kb.matches(data, "tui.select.down") {
            if self.filtered.is_empty() { return; }
            self.selected_index = if kb.matches(data, "tui.select.up") { if self.selected_index == 0 { self.filtered.len() - 1 } else { self.selected_index - 1 } } else { (self.selected_index + 1) % self.filtered.len() };
        } else if kb.matches(data, "app.models.reorderUp") || kb.matches(data, "app.models.reorderDown") {
            let up = kb.matches(data, "app.models.reorderUp");
            if let Some(ids) = &self.enabled_ids && let Some(id) = self.filtered.get(self.selected_index) && let Some(index) = ids.iter().position(|candidate| candidate == id) {
                let next = if up { index.checked_sub(1) } else { index.checked_add(1).filter(|next| *next < ids.len()) };
                if next.is_some() { self.enabled_ids = move_favorite_model(&self.enabled_ids, id, if up { -1 } else { 1 }); self.selected_index = if up { self.selected_index.saturating_sub(1) } else { self.selected_index + 1 }; self.changed(); }
            }
        } else if kb.matches(data, "tui.select.confirm") {
            if let Some(id) = self.filtered.get(self.selected_index).cloned() {
                match &mut self.enabled_ids {
                    None => self.enabled_ids = Some(vec![id]),
                    Some(ids) => if let Some(index) = ids.iter().position(|candidate| candidate == &id) { ids.remove(index); } else { ids.push(id); },
                }
                self.changed();
            }
        } else if kb.matches(data, "app.models.enableAll") || kb.matches(data, "app.models.clearAll") {
            let targets = if self.search_input.get_value().is_empty() { self.all_ids.clone() } else { self.filtered.clone() };
            if kb.matches(data, "app.models.enableAll") { self.enable(&targets); }
            else { self.enabled_ids = clear_favorite_models(&self.enabled_ids, &self.all_ids, if self.search_input.get_value().is_empty() { None } else { Some(&targets) }); }
            self.changed();
        } else if kb.matches(data, "app.models.toggleProvider") {
            if let Some(model) = self.filtered.get(self.selected_index).and_then(|id| self.model(id)) {
                let targets: Vec<_> = self.models.iter().filter(|item| item.provider == model.provider).map(|item| format!("{}/{}", item.provider, item.id)).collect();
                if targets.iter().all(|id| is_favorite_model(&self.enabled_ids, id)) { self.enabled_ids = clear_favorite_models(&self.enabled_ids, &self.all_ids, Some(&targets)); }
                else { self.enable(&targets); }
                self.changed();
            }
        } else if kb.matches(data, "app.models.save") { (self.callbacks.on_persist)(self.enabled_ids.clone()); self.dirty = false; }
        else if matches_key(data, "ctrl+c") { if self.search_input.get_value().is_empty() { (self.callbacks.on_cancel)(); } else { self.search_input.set_value(""); self.refresh(); } }
        else if matches_key(data, "escape") { (self.callbacks.on_cancel)(); }
        else { self.search_input.handle_input(data); self.refresh(); }
    }
    fn has_input_handler(&self) -> bool { true }
}
impl Focusable for ScopedModelsSelectorComponent {
    fn focused(&self) -> bool { self.search_input.focused() }
    fn set_focused(&mut self, value: bool) { self.search_input.set_focused(value); }
}
