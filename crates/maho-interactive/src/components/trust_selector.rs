use std::sync::Arc;
use maho_core::trust_manager::{ProjectTrustOption, ProjectTrustStoreEntry, ProjectTrustUpdate, get_project_trust_options};
use maho_tui::{components::text::Text, keybindings::KeybindingsManager, tui::Component};
use crate::theme::{Theme, ThemeColor};
use super::{keybinding_hints::{key_hint, raw_key_hint}, theme_selector::dynamic_border};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustSelection { pub trusted: bool, pub updates: Vec<ProjectTrustUpdate> }

pub struct TrustSelectorOptions {
    pub cwd: String,
    pub saved_decision: Option<ProjectTrustStoreEntry>,
    pub project_trusted: bool,
    pub on_select: Box<dyn FnMut(TrustSelection)>,
    pub on_cancel: Box<dyn FnMut()>,
}

pub struct TrustSelectorComponent {
    theme: Theme,
    keybindings: Arc<KeybindingsManager>,
    options: TrustSelectorOptions,
    trust_options: Vec<ProjectTrustOption>,
    selected_index: usize,
}

fn is_saved(option: &ProjectTrustOption, saved: Option<&ProjectTrustStoreEntry>) -> bool {
    saved.is_some_and(|saved| saved.decision == option.trusted && option.saved_path.as_deref() == Some(&saved.path))
}

impl TrustSelectorComponent {
    pub fn new(theme: &Theme, keybindings: Arc<KeybindingsManager>, options: TrustSelectorOptions) -> Self {
        let trust_options = get_project_trust_options(&options.cwd, false);
        let selected_index = trust_options.iter().position(|option| is_saved(option, options.saved_decision.as_ref())).unwrap_or(0);
        Self { theme: theme.clone(), keybindings, options, trust_options, selected_index }
    }
}

impl Component for TrustSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = vec![dynamic_border(&self.theme, ThemeColor::Border, width), String::new()];
        let mut add = |text| lines.extend(Text::with_padding(text, 1, 0).render(width));
        add(self.theme.fg(ThemeColor::Accent, &self.theme.bold("Project trust")));
        add(self.theme.fg(ThemeColor::Muted, &self.options.cwd));
        lines.push(String::new());
        let decision = match &self.options.saved_decision {
            None => "none".into(),
            Some(saved) => {
                let label = if saved.decision { "trusted" } else { "untrusted" };
                if self.trust_options.first().and_then(|option| option.saved_path.as_ref()).is_some_and(|path| path != &saved.path) {
                    format!("{label} (inherited from {})", saved.path)
                } else { format!("{label} ({})", saved.path) }
            }
        };
        for text in [format!("Saved decision: {decision}"), format!("Current session: {}", if self.options.project_trusted { "trusted" } else { "untrusted" })] {
            lines.extend(Text::with_padding(self.theme.fg(ThemeColor::Muted, &text), 1, 0).render(width));
        }
        lines.push(String::new());
        for (index, option) in self.trust_options.iter().enumerate() {
            let selected = index == self.selected_index;
            let prefix = if selected { self.theme.fg(ThemeColor::Accent, "→ ") } else { "  ".into() };
            let current = if is_saved(option, self.options.saved_decision.as_ref()) { self.theme.fg(ThemeColor::Accent, "✓ ") } else { "  ".into() };
            let label = self.theme.fg(if selected { ThemeColor::Accent } else { ThemeColor::Text }, &option.label);
            lines.extend(Text::with_padding(format!("{prefix}{current}{label}"), 1, 0).render(width));
        }
        lines.push(String::new());
        let hints = format!("{}  {}  {}", raw_key_hint("↑↓", "navigate", &self.theme), key_hint("tui.select.confirm", "save", &self.theme), key_hint("tui.select.cancel", "cancel", &self.theme));
        lines.extend(Text::with_padding(hints, 1, 0).render(width));
        lines.push(String::new());
        lines.push(dynamic_border(&self.theme, ThemeColor::Border, width));
        lines
    }
    fn handle_input(&mut self, data: &str) {
        if self.keybindings.matches(data, "tui.select.up") || data == "k" { self.selected_index = self.selected_index.saturating_sub(1); }
        else if self.keybindings.matches(data, "tui.select.down") || data == "j" { self.selected_index = (self.selected_index + 1).min(self.trust_options.len().saturating_sub(1)); }
        else if self.keybindings.matches(data, "tui.select.confirm") || data == "\n" {
            if let Some(selected) = self.trust_options.get(self.selected_index) { (self.options.on_select)(TrustSelection { trusted: selected.trusted, updates: selected.updates.clone() }); }
        } else if self.keybindings.matches(data, "tui.select.cancel") { (self.options.on_cancel)(); }
    }
    fn has_input_handler(&self) -> bool { true }
}
