//! Port of senpi `packages/coding-agent/src/modes/interactive/grok/tool-row.ts`.

use maho_tui::tui::Component;

use crate::theme::Theme;

use super::chrome_tokens::get_grok_chrome_tokens;
use super::palette::glyphs;

pub struct GrokToolRowState {
    pub tool_name: String,
    pub is_partial: bool,
    pub is_error: Option<bool>,
}

pub struct GrokToolRow {
    state: GrokToolRowState,
    theme: Theme,
}

impl GrokToolRow {
    pub fn new(state: GrokToolRowState, theme: Theme) -> Self {
        Self { state, theme }
    }

    pub fn update(&mut self, state: GrokToolRowState) {
        self.state = state;
    }
}

impl Component for GrokToolRow {
    fn render(&mut self, _width: usize) -> Vec<String> {
        let tokens = get_grok_chrome_tokens(&self.theme);
        let marker = if self.state.is_partial {
            (tokens.warning)(glyphs::SPINNER)
        } else {
            match self.state.is_error {
                Some(true) => (tokens.error)(glyphs::TOOL_ROW_MARKER),
                Some(false) => (tokens.success)(glyphs::TOOL_ROW_MARKER),
                None => (tokens.warning)(glyphs::TOOL_ROW_MARKER),
            }
        };
        vec![format!(
            "{} {} {}",
            (tokens.muted_text)(glyphs::TOOL_ROW_GUIDE),
            marker,
            (tokens.primary_text)(&self.state.tool_name)
        )]
    }

    fn invalidate(&mut self) {}
}
