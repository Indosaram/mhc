//! Port of senpi `packages/coding-agent/src/modes/interactive/grok/welcome-card.ts`.

use maho_tui::tui::Component;
use maho_tui::utils::{truncate_to_width, visible_width};

use crate::theme::Theme;
use crate::version_label::format_display_version;

use super::chrome_tokens::get_grok_chrome_tokens;

pub struct GrokWelcomeCard {
    app_name: String,
    version: String,
    theme: Theme,
}

impl GrokWelcomeCard {
    pub fn new(app_name: impl Into<String>, version: impl Into<String>, theme: Theme) -> Self {
        Self {
            app_name: app_name.into(),
            version: version.into(),
            theme,
        }
    }
}

impl Component for GrokWelcomeCard {
    fn render(&mut self, width: usize) -> Vec<String> {
        let tokens = get_grok_chrome_tokens(&self.theme);
        let title = format!("{} {}", self.app_name, format_display_version(&self.version));
        if width < 3 {
            return vec![(tokens.primary_text)(&truncate_to_width(&title, width, "", false))];
        }

        let content_width = width - 2;
        let line = |text: &str| {
            let truncated = truncate_to_width(text, content_width, "", false);
            let padding = content_width.saturating_sub(visible_width(&truncated));
            format!("{truncated}{}", " ".repeat(padding))
        };
        let interior = |text: String| {
            format!(
                "{}{}{}",
                (tokens.card_border)("│"),
                (tokens.input_interior)(&text),
                (tokens.card_border)("│")
            )
        };

        vec![
            (tokens.card_border)(&format!("╭{}╮", "─".repeat(content_width))),
            interior(line(&format!(" {}", (tokens.primary_text)(&title)))),
            interior(line(" Ready for your next task.")),
            (tokens.card_border)(&format!("╰{}╯", "─".repeat(content_width))),
        ]
    }

    fn invalidate(&mut self) {}
}
