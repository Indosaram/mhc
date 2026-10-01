//! Port of components/favorite-models-selector.ts.
//!
//! Management surface for the Ctrl+P favorite-model cycle: changes are session-only until the user
//! persists them. The keybindings manager is injected (senpi reads the process-global one) so the
//! `app.models.*` ids the component uses resolve deterministically.

use std::collections::BTreeMap;
use std::sync::Arc;

use maho_tui::components::input::{Input, InputOptions};
use maho_tui::components::text::Text;
use maho_tui::components::truncated_text::TruncatedText;
use maho_tui::keybindings::KeybindingsManager;
use maho_tui::keys::matches_key;
use maho_tui::tui::{Component, Focusable};

use super::favorite_model_ids::{
    FavoriteModelIds, clear_favorite_models, favorite_models, get_model_full_id,
    get_sorted_favorite_model_ids, is_favorite_model, move_favorite_model, toggle_favorite_model,
};
use super::keybinding_hints::key_text;
use super::model_selector::{ModelEntry, models_are_equal};
use super::theme_selector::dynamic_border;
use crate::model_search::ModelSearchItem;
use crate::model_search_rank::rank_model_search_items;
use crate::theme::theme::{Theme, ThemeColor};

#[derive(Debug, Clone)]
struct ModelItem {
    full_id: String,
    model: ModelEntry,
    favorite: bool,
}

pub struct FavoriteModelsConfig {
    pub all_models: Vec<ModelEntry>,
    pub favorite_model_ids: FavoriteModelIds,
    pub current_model: Option<ModelEntry>,
}

pub struct FavoriteModelsCallbacks {
    /// Called whenever the favorite model set or order changes (session-only, no persist).
    pub on_change: Box<dyn FnMut(FavoriteModelIds)>,
    /// Called when the user wants to persist the current selection to settings.
    pub on_persist: Box<dyn FnMut(FavoriteModelIds)>,
    pub on_select: Box<dyn FnMut(&ModelEntry)>,
    pub on_cancel: Box<dyn FnMut()>,
}

/// Component for managing favorite models for Ctrl+P cycling.
pub struct FavoriteModelsSelectorComponent {
    theme: Theme,
    keybindings: Arc<KeybindingsManager>,
    models_by_id: BTreeMap<String, ModelEntry>,
    all_ids: Vec<String>,
    favorite_ids: FavoriteModelIds,
    /// Row order frozen at construction; membership toggles never move rows, only reorder keys do.
    display_ids: Vec<String>,
    filtered_items: Vec<ModelItem>,
    selected_index: usize,
    search_input: Input,
    current_model: Option<ModelEntry>,
    max_visible: usize,
    is_dirty: bool,
    focused: bool,
    callbacks: FavoriteModelsCallbacks,
}

impl FavoriteModelsSelectorComponent {
    pub fn new(
        theme: &Theme,
        keybindings: Arc<KeybindingsManager>,
        config: FavoriteModelsConfig,
        callbacks: FavoriteModelsCallbacks,
    ) -> Self {
        let mut models_by_id = BTreeMap::new();
        let mut all_ids = Vec::new();
        for model in &config.all_models {
            let full_id = model.full_id();
            models_by_id.insert(full_id.clone(), model.clone());
            all_ids.push(full_id);
        }
        let favorite_ids = config.favorite_model_ids;
        let display_ids = get_sorted_favorite_model_ids(&favorite_ids, &all_ids);
        let mut component = Self {
            theme: theme.clone(),
            keybindings,
            models_by_id,
            all_ids,
            favorite_ids,
            display_ids,
            filtered_items: Vec::new(),
            selected_index: 0,
            search_input: Input::new(InputOptions::default()),
            current_model: config.current_model,
            max_visible: 8,
            is_dirty: false,
            focused: false,
            callbacks,
        };
        component.filtered_items = component.build_items();
        component.update_list();
        component
    }

    pub fn search_input(&self) -> &Input {
        &self.search_input
    }

    pub fn favorite_ids(&self) -> &FavoriteModelIds {
        &self.favorite_ids
    }

    pub fn is_dirty(&self) -> bool {
        self.is_dirty
    }

    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub fn filtered_ids(&self) -> Vec<String> {
        self.filtered_items.iter().map(|item| item.full_id.clone()).collect()
    }

    pub fn footer_text(&self) -> String {
        self.get_footer_text()
    }

    pub fn secondary_footer_text(&self) -> String {
        self.get_secondary_footer_text()
    }

    fn build_items(&self) -> Vec<ModelItem> {
        let mut items = Vec::new();
        for id in &self.display_ids {
            let Some(model) = self.models_by_id.get(id) else {
                continue;
            };
            items.push(ModelItem {
                full_id: id.clone(),
                model: model.clone(),
                favorite: is_favorite_model(&self.favorite_ids, id),
            });
        }
        items
    }

    fn get_footer_text(&self) -> String {
        let favorite_count = self.favorite_ids.as_ref().map_or(self.all_ids.len(), Vec::len);
        let all_favorite = self.favorite_ids.is_none();
        let count_text = if all_favorite {
            "all favorites".to_owned()
        } else {
            format!("{favorite_count}/{} favorites", self.all_ids.len())
        };
        let parts = [
            format!("{} select", key_text("tui.select.confirm")),
            format!("{} favorite", key_text("app.models.toggleFavorite")),
            format!("{} save", key_text("app.models.save")),
            count_text,
        ];
        let joined = parts.join(" · ");
        if self.is_dirty {
            format!(
                "{}{}",
                self.theme.fg(ThemeColor::Dim, &format!("  {joined} ")),
                self.theme.fg(ThemeColor::Warning, "unsaved")
            )
        } else {
            self.theme.fg(ThemeColor::Dim, &format!("  {joined}"))
        }
    }

    fn get_secondary_footer_text(&self) -> String {
        let parts = [
            format!("{} all", key_text("app.models.enableAll")),
            format!("{} clear", key_text("app.models.clearAll")),
            format!("{} provider", key_text("app.models.toggleProvider")),
            format!(
                "{}/{} order",
                key_text("app.models.reorderUp"),
                key_text("app.models.reorderDown")
            ),
        ];
        self.theme.fg(ThemeColor::Dim, &format!("  {}", parts.join(" · ")))
    }

    fn refresh(&mut self, preferred_selected_id: Option<&str>) {
        let query = self.search_input.get_value().to_owned();
        let selected_id = preferred_selected_id
            .map(str::to_owned)
            .or_else(|| self.filtered_items.get(self.selected_index).map(|item| item.full_id.clone()));
        let items = self.build_items();
        let search_items: Vec<ModelSearchItem> = items
            .iter()
            .map(|item| ModelSearchItem {
                id: item.model.id.clone(),
                provider: item.model.provider.clone(),
                name: Some(item.model.name.clone()),
            })
            .collect();
        let ranked = rank_model_search_items(&search_items, &query, Clone::clone, false, |_| false);
        self.filtered_items = ranked
            .into_iter()
            .map(|item| ModelItem {
                full_id: get_model_full_id(item),
                model: ModelEntry {
                    provider: item.provider.clone(),
                    id: item.id.clone(),
                    name: item.name.clone().unwrap_or_default(),
                },
                favorite: is_favorite_model(&self.favorite_ids, &get_model_full_id(item)),
            })
            .collect();
        self.selected_index = selected_id
            .and_then(|id| self.filtered_items.iter().position(|item| item.full_id == id))
            .unwrap_or_else(|| self.selected_index.min(self.filtered_items.len().saturating_sub(1)));
        self.update_list();
    }

    /// Mirror a favoriteIds reorder swap onto the frozen display order so the row visibly moves.
    fn swap_display_ids(&mut self, first_id: &str, second_id: &str) {
        let first_index = self.display_ids.iter().position(|id| id == first_id);
        let second_index = self.display_ids.iter().position(|id| id == second_id);
        if let (Some(first_index), Some(second_index)) = (first_index, second_index) {
            self.display_ids.swap(first_index, second_index);
        }
    }

    fn notify_change(&mut self) {
        let next = self.favorite_ids.clone();
        (self.callbacks.on_change)(next);
    }

    fn update_list(&mut self) {
        self.selected_index = self.selected_index.min(self.filtered_items.len().saturating_sub(1));
    }

    fn visible_range(&self) -> (usize, usize) {
        let start_index = self
            .selected_index
            .saturating_sub(self.max_visible / 2)
            .min(self.filtered_items.len().saturating_sub(self.max_visible));
        let end_index = (start_index + self.max_visible).min(self.filtered_items.len());
        (start_index, end_index)
    }

    fn toggle_favorite(&mut self, full_id: &str) {
        self.favorite_ids = toggle_favorite_model(&self.favorite_ids, &self.all_ids, full_id);
        self.is_dirty = true;
        self.refresh(Some(full_id));
        self.notify_change();
    }

    fn apply_targets(&mut self, target_ids: Option<Vec<String>>, enable: bool) {
        let targets = target_ids.as_deref();
        self.favorite_ids = if enable {
            favorite_models(&self.favorite_ids, &self.all_ids, targets)
        } else {
            clear_favorite_models(&self.favorite_ids, &self.all_ids, targets)
        };
        self.is_dirty = true;
        let preferred = self.filtered_items.get(self.selected_index).map(|item| item.full_id.clone());
        self.refresh(preferred.as_deref());
        self.notify_change();
    }

    fn reorder(&mut self, delta: i64) {
        let Some(item) = self.filtered_items.get(self.selected_index).cloned() else {
            return;
        };
        let Some(ids) = self.favorite_ids.clone() else {
            return;
        };
        if !is_favorite_model(&Some(ids.clone()), &item.full_id) {
            return;
        }
        let Some(current_index) = ids.iter().position(|id| id == &item.full_id) else {
            return;
        };
        let new_index = current_index as i64 + delta;
        if new_index < 0 || new_index as usize >= ids.len() {
            return;
        }
        let displaced_id = ids[new_index as usize].clone();
        self.favorite_ids = move_favorite_model(&self.favorite_ids, &item.full_id, delta);
        self.swap_display_ids(&item.full_id, &displaced_id);
        self.is_dirty = true;
        self.selected_index = (self.selected_index as i64 + delta).max(0) as usize;
        self.refresh(Some(&item.full_id));
        self.notify_change();
    }
}

impl Component for FavoriteModelsSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = vec![dynamic_border(&self.theme, ThemeColor::Border, width), String::new()];
        lines.extend(
            Text::with_padding(
                self.theme.fg(ThemeColor::Accent, &self.theme.bold("Favorite Models")),
                0,
                0,
            )
            .render(width),
        );
        let hint = format!(
            "{} selects. {} toggles favorite. {} saves.",
            key_text("tui.select.confirm"),
            key_text("app.models.toggleFavorite"),
            key_text("app.models.save")
        );
        lines.extend(Text::with_padding(self.theme.fg(ThemeColor::Muted, &hint), 0, 0).render(width));
        lines.push(String::new());
        lines.extend(self.search_input.render(width));
        lines.push(String::new());

        if self.filtered_items.is_empty() {
            lines.extend(
                Text::with_padding(self.theme.fg(ThemeColor::Muted, "  No matching models"), 0, 0).render(width),
            );
        } else {
            let (start_index, end_index) = self.visible_range();
            for index in start_index..end_index {
                let item = &self.filtered_items[index];
                let is_selected = index == self.selected_index;
                let prefix = if is_selected {
                    self.theme.fg(ThemeColor::Accent, "→ ")
                } else {
                    "  ".to_owned()
                };
                let favorite_marker = if item.favorite {
                    self.theme.fg(ThemeColor::Success, "*")
                } else {
                    self.theme.fg(ThemeColor::Dim, "-")
                };
                let model_text = if is_selected {
                    self.theme.fg(ThemeColor::Accent, &item.model.id)
                } else {
                    item.model.id.clone()
                };
                let provider_badge = self.theme.fg(ThemeColor::Muted, &format!(" [{}]", item.model.provider));
                let current_marker = if models_are_equal(self.current_model.as_ref(), Some(&item.model)) {
                    self.theme.fg(ThemeColor::Success, " ✓")
                } else {
                    String::new()
                };
                lines.extend(
                    TruncatedText::new(format!("{prefix}{favorite_marker} {model_text}{provider_badge}{current_marker}"))
                        .render(width),
                );
            }

            if start_index > 0 || end_index < self.filtered_items.len() {
                lines.extend(
                    Text::with_padding(
                        self.theme.fg(
                            ThemeColor::Muted,
                            &format!("  ({}/{})", self.selected_index + 1, self.filtered_items.len()),
                        ),
                        0,
                        0,
                    )
                    .render(width),
                );
            }

            lines.push(String::new());
            let name = self.filtered_items[self.selected_index].model.name.clone();
            lines.extend(
                Text::with_padding(self.theme.fg(ThemeColor::Muted, &format!("  Model Name: {name}")), 0, 0)
                    .render(width),
            );
        }

        lines.push(String::new());
        let footer = self.get_footer_text();
        lines.extend(Text::with_padding(footer, 0, 0).render(width));
        let secondary = self.get_secondary_footer_text();
        lines.extend(Text::with_padding(secondary, 0, 0).render(width));
        lines.push(dynamic_border(&self.theme, ThemeColor::Border, width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = &self.keybindings;
        if kb.matches(data, "tui.select.up") {
            if self.filtered_items.is_empty() {
                return;
            }
            self.selected_index = if self.selected_index == 0 {
                self.filtered_items.len() - 1
            } else {
                self.selected_index - 1
            };
            self.update_list();
            return;
        }
        if kb.matches(data, "tui.select.down") {
            if self.filtered_items.is_empty() {
                return;
            }
            self.selected_index = if self.selected_index == self.filtered_items.len() - 1 {
                0
            } else {
                self.selected_index + 1
            };
            self.update_list();
            return;
        }

        let reorder_up = kb.matches(data, "app.models.reorderUp");
        let reorder_down = kb.matches(data, "app.models.reorderDown");
        if reorder_up || reorder_down {
            self.reorder(if reorder_up { -1 } else { 1 });
            return;
        }

        if kb.matches(data, "tui.select.confirm") {
            if let Some(item) = self.filtered_items.get(self.selected_index) {
                let model = item.model.clone();
                (self.callbacks.on_select)(&model);
            }
            return;
        }

        if kb.matches(data, "app.models.toggleFavorite") {
            if let Some(item) = self.filtered_items.get(self.selected_index).cloned() {
                self.toggle_favorite(&item.full_id);
            }
            return;
        }

        if kb.matches(data, "app.models.enableAll") {
            let targets = if self.search_input.get_value().is_empty() {
                None
            } else {
                Some(self.filtered_items.iter().map(|item| item.full_id.clone()).collect())
            };
            self.apply_targets(targets, true);
            return;
        }

        if kb.matches(data, "app.models.clearAll") {
            let targets = if self.search_input.get_value().is_empty() {
                None
            } else {
                Some(self.filtered_items.iter().map(|item| item.full_id.clone()).collect())
            };
            self.apply_targets(targets, false);
            return;
        }

        if kb.matches(data, "app.models.toggleProvider") {
            if let Some(item) = self.filtered_items.get(self.selected_index).cloned() {
                let provider = item.model.provider.clone();
                let provider_ids: Vec<String> = self
                    .all_ids
                    .iter()
                    .filter(|id| {
                        self.models_by_id
                            .get(*id)
                            .is_some_and(|model| model.provider == provider)
                    })
                    .cloned()
                    .collect();
                let all_favorite = provider_ids
                    .iter()
                    .all(|id| is_favorite_model(&self.favorite_ids, id));
                self.apply_targets(Some(provider_ids), !all_favorite);
            }
            return;
        }

        if kb.matches(data, "app.models.save") {
            let next = self.favorite_ids.clone();
            (self.callbacks.on_persist)(next);
            self.is_dirty = false;
            return;
        }

        if matches_key(data, "ctrl+c") {
            if self.search_input.get_value().is_empty() {
                (self.callbacks.on_cancel)();
            } else {
                self.search_input.set_value("");
                self.refresh(None);
            }
            return;
        }

        if matches_key(data, "escape") {
            (self.callbacks.on_cancel)();
            return;
        }

        self.search_input.handle_input(data);
        self.refresh(None);
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn focusable_get(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn focusable_set(&mut self, focused: bool) {
        self.focused = focused;
        self.search_input.set_focused(focused);
    }

    fn invalidate(&mut self) {
        self.search_input.invalidate();
    }
}

impl Focusable for FavoriteModelsSelectorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
        self.search_input.set_focused(value);
    }
}
