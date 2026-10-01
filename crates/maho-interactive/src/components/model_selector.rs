//! Port of components/model-selector.ts.
//!
//! senpi reads the catalog from a `ModelRuntime`/`ModelRegistry` (plan todos 17/21) and persists the
//! default with a `SettingsManager`. This port keeps the component logic — snapshot load, the
//! all/narrowed scope toggle, ranking, windowed list, favorite toggling and the refresh status
//! messages — and takes the runtime and settings side effects as host callbacks, so todo 35 wires
//! the real runtime without the component importing an unported module.
//!
//! senpi's `sortModels` compares provider and id with `localeCompare` (ICU collation, which ignores
//! punctuation); this port uses Rust's code-point ordering. The two agree for the ASCII ids the
//! catalog uses today and differ only for ids that collate equal under ICU.

use std::time::Duration;

use maho_ai::models::ModelsRefreshResult;
use maho_ai::utils::abort::{AbortController, AbortSignal};
use maho_tui::components::input::{Input, InputOptions};
use maho_tui::components::text::Text;
use maho_tui::keybindings::KeybindingsManager;
use maho_tui::tui::{Component, Focusable};
use std::sync::Arc;

use super::favorite_model_ids::{
    FavoriteModelIds, get_model_full_id, is_favorite_model, toggle_favorite_model,
};
use super::keybinding_hints::key_hint;
use super::theme_selector::dynamic_border;
use crate::model_catalog_refresh::refresh_model_catalogs;
use crate::model_search::ModelSearchItem;
use crate::model_search_rank::rank_model_search_items;
use crate::theme::theme::{Theme, ThemeColor};

/// senpi's `Model` identity used for equality and display: `modelsAreEqual` compares id+provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    pub provider: String,
    pub id: String,
    pub name: String,
}

impl ModelEntry {
    pub fn full_id(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

/// senpi's `modelsAreEqual(a, b)`.
pub fn models_are_equal(a: Option<&ModelEntry>, b: Option<&ModelEntry>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.id == b.id && a.provider == b.provider,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedModelItem {
    pub model: ModelEntry,
    pub thinking_level: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelScope {
    All,
    Narrowed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelItem {
    full_id: String,
    provider: String,
    id: String,
    name: String,
}

impl ModelItem {
    fn from_entry(entry: &ModelEntry) -> Self {
        Self {
            full_id: entry.full_id(),
            provider: entry.provider.clone(),
            id: entry.id.clone(),
            name: entry.name.clone(),
        }
    }

    fn matches(&self, entry: Option<&ModelEntry>) -> bool {
        entry.is_some_and(|entry| entry.id == self.id && entry.provider == self.provider)
    }

    fn as_search_item(&self) -> ModelSearchItem {
        ModelSearchItem {
            id: self.id.clone(),
            provider: self.provider.clone(),
            name: Some(self.name.clone()),
        }
    }
}

/// senpi's `onFavoriteChange`.
pub type FavoriteChangeHandler = Box<dyn FnMut(FavoriteModelIds, &[ModelEntry], &ModelEntry)>;
/// senpi's `settingsManager.setDefaultModelAndProvider` call.
pub type DefaultModelChangeHandler = Box<dyn FnMut(&ModelEntry)>;

pub struct ModelSelectorFavoriteOptions {
    pub favorite_model_ids: FavoriteModelIds,
    pub on_favorite_change: Option<FavoriteChangeHandler>,
}

/// Component that renders a model selector with search.
pub struct ModelSelectorComponent {
    theme: Theme,
    keybindings: Arc<KeybindingsManager>,
    search_input: Input,
    all_models: Vec<ModelItem>,
    scoped_model_items: Vec<ModelItem>,
    active_models: Vec<ModelItem>,
    filtered_models: Vec<ModelItem>,
    selected_index: usize,
    current_model: Option<ModelEntry>,
    favorite_ids: FavoriteModelIds,
    /// Membership snapshot taken when the selector opens; ordering is frozen against this basis.
    favorite_ids_at_open: FavoriteModelIds,
    on_favorite_change: Option<FavoriteChangeHandler>,
    error_message: Option<String>,
    refresh_status_message: String,
    refresh_status_success: bool,
    scoped_models: Vec<ScopedModelItem>,
    scope: ModelScope,
    terminal_rows: Option<usize>,
    runtime_key: u64,
    focused: bool,
    closed: bool,
    on_select: Box<dyn FnMut(&ModelEntry)>,
    on_cancel: Box<dyn FnMut()>,
    /// senpi's `settingsManager.setDefaultModelAndProvider`.
    on_default_model_change: Option<DefaultModelChangeHandler>,
    request_render: Option<Box<dyn FnMut()>>,
}

impl ModelSelectorComponent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        theme: &Theme,
        keybindings: Arc<KeybindingsManager>,
        runtime_key: u64,
        available_models: &[ModelEntry],
        current_model: Option<ModelEntry>,
        scoped_models: Vec<ScopedModelItem>,
        on_select: Box<dyn FnMut(&ModelEntry)>,
        on_cancel: Box<dyn FnMut()>,
        initial_search_input: Option<&str>,
        favorites: ModelSelectorFavoriteOptions,
        terminal_rows: Option<usize>,
    ) -> Self {
        let mut search_input = Input::new(InputOptions::default());
        if let Some(initial) = initial_search_input {
            search_input.set_value(initial);
        }
        let scope = if scoped_models.is_empty() {
            ModelScope::All
        } else {
            ModelScope::Narrowed
        };
        let favorite_ids = favorites.favorite_model_ids;
        let favorite_ids_at_open = favorite_ids.clone();
        let mut component = Self {
            theme: theme.clone(),
            keybindings,
            search_input,
            all_models: Vec::new(),
            scoped_model_items: Vec::new(),
            active_models: Vec::new(),
            filtered_models: Vec::new(),
            selected_index: 0,
            current_model,
            favorite_ids,
            favorite_ids_at_open,
            on_favorite_change: favorites.on_favorite_change,
            error_message: None,
            refresh_status_message: "Refreshing model catalogs…".to_owned(),
            refresh_status_success: false,
            scoped_models,
            scope,
            terminal_rows,
            runtime_key,
            focused: false,
            closed: false,
            on_select,
            on_cancel,
            on_default_model_change: None,
            request_render: None,
        };
        component.load_models_from_snapshot(available_models);
        if let Some(initial) = initial_search_input {
            component.filter_models(initial);
        } else {
            component.update_list();
        }
        component
    }

    pub fn set_default_model_change_handler(&mut self, handler: Box<dyn FnMut(&ModelEntry)>) {
        self.on_default_model_change = Some(handler);
    }

    pub fn set_request_render(&mut self, request_render: Box<dyn FnMut()>) {
        self.request_render = Some(request_render);
    }

    pub fn search_input(&self) -> &Input {
        &self.search_input
    }

    pub fn scope(&self) -> ModelScope {
        self.scope
    }

    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub fn filtered_model_ids(&self) -> Vec<String> {
        self.filtered_models.iter().map(|item| item.full_id.clone()).collect()
    }

    pub fn error_message(&self) -> Option<&str> {
        self.error_message.as_deref()
    }

    pub fn refresh_status_message(&self) -> &str {
        &self.refresh_status_message
    }

    pub fn favorite_ids(&self) -> &FavoriteModelIds {
        &self.favorite_ids
    }

    fn request_render(&mut self) {
        if let Some(render) = &mut self.request_render {
            render();
        }
    }

    fn load_models_from_snapshot(&mut self, available_models: &[ModelEntry]) {
        let models: Vec<ModelItem> = available_models.iter().map(ModelItem::from_entry).collect();
        self.all_models = self.sort_models(&models);
        let by_full_id: std::collections::BTreeMap<String, ModelItem> = models
            .iter()
            .map(|item| (item.full_id.clone(), item.clone()))
            .collect();
        self.scoped_models = self
            .scoped_models
            .iter()
            .filter_map(|scoped| {
                available_models
                    .iter()
                    .find(|entry| entry.id == scoped.model.id && entry.provider == scoped.model.provider)
                    .map(|entry| ScopedModelItem {
                        model: entry.clone(),
                        thinking_level: scoped.thinking_level.clone(),
                    })
            })
            .collect();
        self.scoped_model_items = self
            .scoped_models
            .iter()
            .filter_map(|scoped| by_full_id.get(&scoped.model.full_id()).cloned())
            .collect();
        self.active_models = match self.scope {
            ModelScope::Narrowed => self.scoped_model_items.clone(),
            ModelScope::All => self.all_models.clone(),
        };
        self.filtered_models = self.active_models.clone();
        self.selected_index = self
            .filtered_models
            .iter()
            .position(|item| item.matches(self.current_model.as_ref()))
            .unwrap_or_else(|| self.selected_index.min(self.filtered_models.len().saturating_sub(1)));
    }

    /// senpi's `refreshModels`: shared refresh with a 15s timeout, then reload and re-filter.
    pub async fn refresh_models<F, Fut>(&mut self, timeout_ms: u64, start: F)
    where
        F: FnOnce(AbortSignal) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ModelsRefreshResult> + Send + 'static,
    {
        let controller = AbortController::new();
        let signal = controller.signal();
        let mut timed_out = false;
        let outcome = tokio::select! {
            biased;
            result = refresh_model_catalogs(self.runtime_key, &signal, start) => result.ok(),
            () = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
                timed_out = true;
                controller.abort(None);
                None
            }
        };
        if self.closed {
            return;
        }
        let result = outcome.unwrap_or(ModelsRefreshResult { aborted: timed_out, errors: Default::default() });
        self.refresh_status_message = String::new();
        if result.aborted && timed_out {
            self.error_message = Some("Model refresh timed out; showing cached models.".to_owned());
        } else if result.errors.len() == 1 {
            let provider = result.errors.keys().next().cloned().unwrap_or_default();
            self.error_message = Some(format!("Could not refresh {provider}; showing cached models."));
        } else if result.errors.len() > 1 {
            let providers: Vec<String> = result.errors.keys().cloned().collect();
            self.error_message = Some(format!(
                "Could not refresh {} model catalogs ({}); showing cached models.",
                result.errors.len(),
                providers.join(", ")
            ));
        } else {
            self.refresh_status_message = "Model catalogs refreshed.".to_owned();
            self.refresh_status_success = true;
        }
        let query = self.search_input.get_value().to_owned();
        self.filter_models(&query);
        self.request_render();
    }

    /// Record a refresh failure raised by the host (senpi's `catch` branch).
    pub fn set_refresh_failure(&mut self, error: &str, timed_out: bool) {
        if self.closed {
            return;
        }
        self.refresh_status_message = String::new();
        self.error_message = Some(if timed_out {
            "Model refresh timed out; showing cached models.".to_owned()
        } else {
            format!("Could not refresh model catalogs: {error}")
        });
        self.update_list();
        self.request_render();
    }

    pub fn dispose(&mut self) {
        self.closed = true;
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    fn sort_models(&self, models: &[ModelItem]) -> Vec<ModelItem> {
        let mut sorted = models.to_vec();
        sorted.sort_by(|a, b| {
            let a_is_current = a.matches(self.current_model.as_ref());
            let b_is_current = b.matches(self.current_model.as_ref());
            if a_is_current != b_is_current {
                return if a_is_current { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater };
            }
            let a_is_favorite = is_favorite_model(&self.favorite_ids_at_open, &a.full_id);
            let b_is_favorite = is_favorite_model(&self.favorite_ids_at_open, &b.full_id);
            if a_is_favorite != b_is_favorite {
                return if a_is_favorite { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater };
            }
            a.provider.cmp(&b.provider).then_with(|| a.id.cmp(&b.id))
        });
        sorted
    }

    fn scope_text(&self) -> String {
        let all_text = match self.scope {
            ModelScope::All => self.theme.fg(ThemeColor::Accent, "all"),
            ModelScope::Narrowed => self.theme.fg(ThemeColor::Muted, "all"),
        };
        let narrowed_text = match self.scope {
            ModelScope::Narrowed => self.theme.fg(ThemeColor::Accent, "narrowed"),
            ModelScope::All => self.theme.fg(ThemeColor::Muted, "narrowed"),
        };
        format!(
            "{}{}{}{}",
            self.theme.fg(ThemeColor::Muted, "Catalog: "),
            all_text,
            self.theme.fg(ThemeColor::Muted, " | "),
            narrowed_text
        )
    }

    fn scope_hint_text(&self) -> String {
        format!(
            "{}{}",
            key_hint("tui.input.tab", "catalog", &self.theme),
            self.theme.fg(ThemeColor::Muted, " (all/narrowed)")
        )
    }

    fn set_scope(&mut self, scope: ModelScope) {
        if self.scope == scope {
            return;
        }
        self.scope = scope;
        self.active_models = match scope {
            ModelScope::Narrowed => self.scoped_model_items.clone(),
            ModelScope::All => self.all_models.clone(),
        };
        self.selected_index = self
            .active_models
            .iter()
            .position(|item| item.matches(self.current_model.as_ref()))
            .unwrap_or(0);
        let query = self.search_input.get_value().to_owned();
        self.filter_models(&query);
    }

    fn filter_models(&mut self, query: &str) {
        self.filtered_models = if query.is_empty() {
            self.active_models.clone()
        } else {
            let search_items: Vec<ModelSearchItem> =
                self.active_models.iter().map(|item| item.as_search_item()).collect();
            rank_model_search_items(
                &search_items,
                query,
                Clone::clone,
                self.scope == ModelScope::All,
                |item| is_favorite_model(&self.favorite_ids_at_open, &get_model_full_id(item)),
            )
            .into_iter()
            .map(|item| ModelItem {
                full_id: get_model_full_id(item),
                provider: item.provider.clone(),
                id: item.id.clone(),
                name: item.name.clone().unwrap_or_default(),
            })
            .collect()
        };
        self.selected_index = if query.is_empty() {
            self.selected_index.min(self.filtered_models.len().saturating_sub(1))
        } else {
            0
        };
        self.update_list();
    }

    /// senpi's `updateList`: it rebuilds the list container; the rows are produced by `render`.
    fn update_list(&mut self) {
        self.selected_index = self.selected_index.min(self.filtered_models.len().saturating_sub(1));
    }

    fn max_visible(&self) -> usize {
        self.terminal_rows.map_or(10, |rows| (rows / 2).max(5))
    }

    fn visible_range(&self) -> (usize, usize) {
        let max_visible = self.max_visible();
        let start_index = self
            .selected_index
            .saturating_sub(max_visible / 2)
            .min(self.filtered_models.len().saturating_sub(max_visible));
        let end_index = (start_index + max_visible).min(self.filtered_models.len());
        (start_index, end_index)
    }

    fn handle_select(&mut self, item: ModelItem) {
        self.dispose();
        let entry = ModelEntry {
            provider: item.provider.clone(),
            id: item.id.clone(),
            name: item.name.clone(),
        };
        if let Some(handler) = &mut self.on_default_model_change {
            handler(&entry);
        }
        (self.on_select)(&entry);
    }

    fn handle_toggle_favorite(&mut self) {
        let Some(selected) = self.filtered_models.get(self.selected_index).cloned() else {
            return;
        };
        let all_model_ids: Vec<String> = self.all_models.iter().map(|item| item.full_id.clone()).collect();
        self.favorite_ids = toggle_favorite_model(&self.favorite_ids, &all_model_ids, &selected.full_id);
        let query = self.search_input.get_value().to_owned();
        self.filter_models(&query);
        if let Some(index) = self
            .filtered_models
            .iter()
            .position(|item| item.full_id == selected.full_id)
        {
            self.selected_index = index;
            self.update_list();
        }
        let next_favorite_ids = self.favorite_ids.clone();
        let all_models: Vec<ModelEntry> = self
            .all_models
            .iter()
            .map(|item| ModelEntry {
                provider: item.provider.clone(),
                id: item.id.clone(),
                name: item.name.clone(),
            })
            .collect();
        let toggled = ModelEntry {
            provider: selected.provider.clone(),
            id: selected.id.clone(),
            name: selected.name.clone(),
        };
        if let Some(callback) = &mut self.on_favorite_change {
            callback(next_favorite_ids, &all_models, &toggled);
        }
        self.request_render();
    }
}

impl Component for ModelSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = vec![dynamic_border(&self.theme, ThemeColor::Border, width), String::new()];

        if !self.scoped_models.is_empty() {
            lines.extend(Text::with_padding(self.scope_text(), 0, 0).render(width));
            lines.extend(Text::with_padding(self.scope_hint_text(), 0, 0).render(width));
        } else {
            let hint = "Only showing models from configured providers. Use /login to add providers.";
            lines.extend(Text::with_padding(self.theme.fg(ThemeColor::Warning, hint), 0, 0).render(width));
        }
        lines.push(String::new());
        let hints = format!(
            "{} {}",
            key_hint("tui.select.confirm", "select", &self.theme),
            key_hint("app.models.toggleFavorite", "favorite", &self.theme)
        );
        lines.extend(Text::with_padding(hints, 0, 0).render(width));
        lines.push(String::new());
        lines.extend(self.search_input.render(width));
        lines.push(String::new());

        let (start_index, end_index) = self.visible_range();
        if start_index > 0 {
            lines.extend(
                Text::with_padding(
                    self.theme.fg(ThemeColor::Muted, &format!("  … {start_index} more above")),
                    0,
                    0,
                )
                .render(width),
            );
        }

        for index in start_index..end_index {
            let item = &self.filtered_models[index];
            let is_selected = index == self.selected_index;
            let is_current = item.matches(self.current_model.as_ref());
            let favorite_marker = if is_favorite_model(&self.favorite_ids, &item.full_id) {
                self.theme.fg(ThemeColor::Success, "* ")
            } else {
                self.theme.fg(ThemeColor::Dim, "  ")
            };
            let provider_badge = self.theme.fg(ThemeColor::Muted, &format!("[{}]", item.provider));
            let checkmark = if is_current {
                self.theme.fg(ThemeColor::Success, " ✓")
            } else {
                String::new()
            };
            let line = if is_selected {
                format!(
                    "{}{}{} {}{}",
                    self.theme.fg(ThemeColor::Accent, "→ "),
                    favorite_marker,
                    self.theme.fg(ThemeColor::Accent, &item.id),
                    provider_badge,
                    checkmark
                )
            } else {
                format!("  {favorite_marker}{} {provider_badge}{checkmark}", item.id)
            };
            lines.extend(Text::with_padding(line, 0, 0).render(width));
        }

        if end_index < self.filtered_models.len() {
            lines.extend(
                Text::with_padding(
                    self.theme.fg(
                        ThemeColor::Muted,
                        &format!("  … {} more below", self.filtered_models.len() - end_index),
                    ),
                    0,
                    0,
                )
                .render(width),
            );
        }

        if let Some(error) = &self.error_message {
            for line in error.split('\n') {
                lines.extend(Text::with_padding(self.theme.fg(ThemeColor::Error, line), 0, 0).render(width));
            }
        } else if self.filtered_models.is_empty() {
            lines.extend(
                Text::with_padding(self.theme.fg(ThemeColor::Muted, "  No matching models"), 0, 0).render(width),
            );
        } else {
            lines.push(String::new());
            let name = self.filtered_models[self.selected_index].name.clone();
            lines.extend(
                Text::with_padding(self.theme.fg(ThemeColor::Muted, &format!("  Model Name: {name}")), 0, 0)
                    .render(width),
            );
        }
        if !self.refresh_status_message.is_empty() {
            lines.push(String::new());
            let color = if self.refresh_status_success {
                ThemeColor::Success
            } else {
                ThemeColor::Muted
            };
            lines.extend(
                Text::with_padding(
                    self.theme.fg(color, &format!("  {}", self.refresh_status_message)),
                    0,
                    0,
                )
                .render(width),
            );
        }

        lines.push(String::new());
        lines.push(dynamic_border(&self.theme, ThemeColor::Border, width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = &self.keybindings;
        if kb.matches(data, "tui.input.tab") {
            if !self.scoped_model_items.is_empty() {
                let next = match self.scope {
                    ModelScope::All => ModelScope::Narrowed,
                    ModelScope::Narrowed => ModelScope::All,
                };
                self.set_scope(next);
            }
            return;
        }
        if kb.matches(data, "tui.select.up") {
            if self.filtered_models.is_empty() {
                return;
            }
            self.selected_index = if self.selected_index == 0 {
                self.filtered_models.len() - 1
            } else {
                self.selected_index - 1
            };
        } else if kb.matches(data, "tui.select.down") {
            if self.filtered_models.is_empty() {
                return;
            }
            self.selected_index = if self.selected_index == self.filtered_models.len() - 1 {
                0
            } else {
                self.selected_index + 1
            };
        } else if kb.matches(data, "tui.select.confirm") {
            if let Some(item) = self.filtered_models.get(self.selected_index).cloned() {
                self.handle_select(item);
            }
        } else if kb.matches(data, "app.models.toggleFavorite") {
            self.handle_toggle_favorite();
        } else if kb.matches(data, "tui.select.cancel") {
            self.dispose();
            (self.on_cancel)();
        } else {
            self.search_input.handle_input(data);
            let query = self.search_input.get_value().to_owned();
            self.filter_models(&query);
        }
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

impl Focusable for ModelSelectorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
        self.search_input.set_focused(value);
    }
}
