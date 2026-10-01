//! Port of components/oauth-selector.ts.

use maho_ai::auth::types::{AuthCheck, AuthType};
use maho_tui::components::input::{Input, InputOptions};
use maho_tui::components::truncated_text::TruncatedText;
use maho_tui::fuzzy::fuzzy_filter;
use maho_tui::keybindings::get_keybindings;
use maho_tui::tui::{Component, Focusable};

use super::theme_selector::dynamic_border;
use crate::theme::theme::{Theme, ThemeColor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthSelectorAuthType {
    OAuth,
    ApiKey,
}

impl AuthSelectorAuthType {
    fn as_str(self) -> &'static str {
        match self {
            Self::OAuth => "oauth",
            Self::ApiKey => "api_key",
        }
    }
}

impl From<AuthType> for AuthSelectorAuthType {
    fn from(value: AuthType) -> Self {
        match value {
            AuthType::OAuth => Self::OAuth,
            AuthType::ApiKey => Self::ApiKey,
        }
    }
}

impl From<AuthSelectorAuthType> for AuthType {
    fn from(value: AuthSelectorAuthType) -> Self {
        match value {
            AuthSelectorAuthType::OAuth => Self::OAuth,
            AuthSelectorAuthType::ApiKey => Self::ApiKey,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuthSelectorProvider {
    pub id: String,
    pub name: String,
    pub auth_type: AuthSelectorAuthType,
    /// senpi's `method?.name` — the credential method's display name.
    pub method_name: Option<String>,
    pub status: Option<AuthCheck>,
}

/// senpi's `formatAuthSelectorProviderType`.
pub fn format_auth_selector_provider_type(auth_type: AuthSelectorAuthType) -> &'static str {
    match auth_type {
        AuthSelectorAuthType::OAuth => "subscription",
        AuthSelectorAuthType::ApiKey => "API key",
    }
}

/// senpi's `onSelect(providerId, authType)`.
pub type ProviderSelectHandler = Box<dyn FnMut(&str, AuthSelectorAuthType)>;
pub type CancelHandler = Box<dyn FnMut()>;

/// Component that renders an auth provider selector.
pub struct OAuthSelectorComponent {
    theme: Theme,
    mode: AuthSelectorMode,
    search_input: Input,
    all_providers: Vec<AuthSelectorProvider>,
    filtered_providers: Vec<AuthSelectorProvider>,
    selected_index: usize,
    show_auth_type_labels: bool,
    focused: bool,
    on_select: ProviderSelectHandler,
    on_cancel: CancelHandler,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthSelectorMode {
    Login,
    Logout,
}

impl OAuthSelectorComponent {
    pub fn new(
        theme: &Theme,
        mode: AuthSelectorMode,
        providers: Vec<AuthSelectorProvider>,
        on_select: ProviderSelectHandler,
        on_cancel: CancelHandler,
        initial_search_input: Option<&str>,
    ) -> Self {
        let mut search_input = Input::new(InputOptions::default());
        if let Some(initial) = initial_search_input {
            search_input.set_value(initial);
        }
        let show_auth_type_labels = providers
            .iter()
            .map(|provider| provider.auth_type.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            > 1;
        let mut component = Self {
            theme: theme.clone(),
            mode,
            search_input,
            all_providers: providers,
            filtered_providers: Vec::new(),
            selected_index: 0,
            show_auth_type_labels,
            focused: false,
            on_select,
            on_cancel,
        };
        component.filter_providers(initial_search_input.unwrap_or(""));
        component
    }

    pub fn search_input(&self) -> &Input {
        &self.search_input
    }

    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub fn filtered_providers(&self) -> &[AuthSelectorProvider] {
        &self.filtered_providers
    }

    fn filter_providers(&mut self, query: &str) {
        self.filtered_providers = if query.is_empty() {
            self.all_providers.clone()
        } else {
            fuzzy_filter(&self.all_providers, query, |provider| {
                format!(
                    "{} {} {} {}",
                    provider.name,
                    provider.id,
                    provider.auth_type.as_str(),
                    provider.method_name.clone().unwrap_or_default()
                )
            })
        };
        self.selected_index = self.selected_index.min(self.filtered_providers.len().saturating_sub(1));
        self.update_list();
    }

    /// senpi's private `updateList`: it rebuilds the list container; in Rust the rows are produced
    /// on demand by `render`, so this only keeps the selection valid.
    fn update_list(&mut self) {
        self.selected_index = self
            .selected_index
            .min(self.filtered_providers.len().saturating_sub(1));
    }

    fn status_indicator(&self, provider: &AuthSelectorProvider) -> String {
        let Some(status) = &provider.status else {
            return self.theme.fg(ThemeColor::Muted, " • unconfigured");
        };
        if status.auth_type != provider.auth_type.into() {
            let label = match status.auth_type {
                AuthType::OAuth => "subscription configured",
                AuthType::ApiKey => "API key configured",
            };
            return format!(
                "{}{}",
                self.theme.fg(ThemeColor::Muted, " • "),
                self.theme.fg(ThemeColor::Warning, label)
            );
        }
        let source = status.source.as_deref();
        if source.is_none() || source == Some("OAuth") || source == Some("stored credential") {
            return self.theme.fg(ThemeColor::Success, " ✓ configured");
        }
        let source = source.unwrap_or_default();
        let label = if is_env_var_list(source) {
            format!("env: {source}")
        } else {
            source.to_owned()
        };
        self.theme.fg(ThemeColor::Success, &format!(" ✓ {label}"))
    }
}

/// senpi's `/^[A-Z][A-Z0-9_]*(?:, [A-Z][A-Z0-9_]*)*$/`.
fn is_env_var_list(source: &str) -> bool {
    fn is_name(part: &str) -> bool {
        let mut chars = part.chars();
        match chars.next() {
            Some(first) if first.is_ascii_uppercase() => {}
            _ => return false,
        }
        chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    }
    !source.is_empty() && source.split(", ").all(is_name)
}

impl Component for OAuthSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = vec![dynamic_border(&self.theme, ThemeColor::Border, width), String::new()];
        let title = match self.mode {
            AuthSelectorMode::Login => "Select provider to configure:",
            AuthSelectorMode::Logout => "Select provider to logout:",
        };
        lines.extend(
            TruncatedText::with_padding(
                self.theme
                    .fg(ThemeColor::Accent, &self.theme.bold(title)),
                1,
                0,
            )
            .render(width),
        );
        lines.push(String::new());
        lines.extend(self.search_input.render(width));
        lines.push(String::new());

        let max_visible = 8usize;
        let start_index = self
            .selected_index
            .saturating_sub(max_visible / 2)
            .min(self.filtered_providers.len().saturating_sub(max_visible));
        let end_index = (start_index + max_visible).min(self.filtered_providers.len());

        for index in start_index..end_index {
            let provider = &self.filtered_providers[index];
            let is_selected = index == self.selected_index;
            let status_indicator = self.status_indicator(provider);
            let auth_type_label = if self.show_auth_type_labels {
                self.theme.fg(
                    ThemeColor::Muted,
                    &format!(" [{}]", format_auth_selector_provider_type(provider.auth_type)),
                )
            } else {
                String::new()
            };
            let line = if is_selected {
                format!(
                    "{}{}{}{}",
                    self.theme.fg(ThemeColor::Accent, "→ "),
                    self.theme.fg(ThemeColor::Accent, &provider.name),
                    auth_type_label,
                    status_indicator
                )
            } else {
                format!(
                    "  {}{}{}",
                    self.theme.fg(ThemeColor::Text, &provider.name),
                    auth_type_label,
                    status_indicator
                )
            };
            lines.extend(TruncatedText::with_padding(line, 1, 0).render(width));
        }

        if start_index > 0 || end_index < self.filtered_providers.len() {
            let scroll_info = self.theme.fg(
                ThemeColor::Muted,
                &format!("  ({}/{})", self.selected_index + 1, self.filtered_providers.len()),
            );
            lines.extend(TruncatedText::with_padding(scroll_info, 1, 0).render(width));
        }

        if self.filtered_providers.is_empty() {
            let message = if self.all_providers.is_empty() {
                match self.mode {
                    AuthSelectorMode::Login => "No providers available",
                    AuthSelectorMode::Logout => "No providers logged in. Use /login first.",
                }
            } else {
                "No matching providers"
            };
            lines.extend(
                TruncatedText::with_padding(
                    self.theme.fg(ThemeColor::Muted, &format!("  {message}")),
                    1,
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
        let kb = get_keybindings();
        if kb.matches(data, "tui.select.up") {
            if self.filtered_providers.is_empty() {
                return;
            }
            self.selected_index = self.selected_index.saturating_sub(1);
        } else if kb.matches(data, "tui.select.down") {
            if self.filtered_providers.is_empty() {
                return;
            }
            self.selected_index = (self.selected_index + 1).min(self.filtered_providers.len() - 1);
        } else if kb.matches(data, "tui.select.confirm") {
            if let Some(provider) = self.filtered_providers.get(self.selected_index) {
                let (id, auth_type) = (provider.id.clone(), provider.auth_type);
                (self.on_select)(&id, auth_type);
            }
        } else if kb.matches(data, "tui.select.cancel") {
            (self.on_cancel)();
        } else {
            self.search_input.handle_input(data);
            let query = self.search_input.get_value().to_owned();
            self.filter_providers(&query);
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

impl Focusable for OAuthSelectorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
        self.search_input.set_focused(value);
    }
}
