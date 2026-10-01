//! Port of senpi `packages/coding-agent/src/modes/interactive/grok/footer.ts`.

use maho_tui::tui::Component;
use maho_tui::utils::truncate_to_width;

use crate::theme::Theme;

use super::chrome_tokens::get_grok_chrome_tokens;

/// The grok footer reads only cwd and model id from the interactive session, which todo 35 owns.
pub trait GrokFooterSession {
    fn cwd(&self) -> String;
    fn model_id(&self) -> Option<String>;
}

pub struct GrokFooter {
    session: std::rc::Rc<dyn GrokFooterSession>,
    theme: Theme,
}

impl GrokFooter {
    pub fn new(session: std::rc::Rc<dyn GrokFooterSession>, theme: Theme) -> Self {
        Self { session, theme }
    }

    pub fn set_session(&mut self, session: std::rc::Rc<dyn GrokFooterSession>) {
        self.session = session;
    }

    pub fn set_auto_compact_enabled(&mut self, _enabled: bool) {}
}

impl Component for GrokFooter {
    fn render(&mut self, width: usize) -> Vec<String> {
        let tokens = get_grok_chrome_tokens(&self.theme);
        let cwd = self.session.cwd();
        let model = self.session.model_id().unwrap_or_else(|| "no-model".to_string());
        vec![truncate_to_width(
            &format!("{} {}", (tokens.cwd)(&cwd), (tokens.model_label)(&model)),
            width,
            "",
            false,
        )]
    }

    fn invalidate(&mut self) {}
}
