//! Port of senpi `packages/coding-agent/src/modes/interactive/grok/chrome.ts`.
//!
//! The `InteractiveChrome` trait is defined here, but four types it names belong to other todos and
//! are not available in this lane: `CustomEditor` and `InteractiveSession` (todo 35),
//! `ToolExecutionPresentation` (todo 32) and `ReadonlyFooterDataProvider` (todo 35). This port keeps
//! the grok-owned classes and expresses those seams with in-crate equivalents; todo 35 wires the
//! real types in. Recorded in `parity.d/34.md`.

use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::components::editor::EditorTheme;
use maho_tui::components::loader::LoaderIndicatorOptions;
use maho_tui::tui::Component;

use crate::components::status_indicator::StatusIndicator;
use crate::theme::{Theme, ThemeColor};

use super::chrome_tokens::get_grok_chrome_tokens;
use super::footer::{GrokFooter, GrokFooterSession};
use super::input_card::GrokInputCard;
use super::palette::glyphs;
use super::welcome_card::GrokWelcomeCard;

pub trait InteractiveFooter: Component {
    fn set_session(&mut self, session: Rc<dyn GrokFooterSession>);
    fn set_auto_compact_enabled(&mut self, enabled: bool);
    fn set_compaction_delegated(&mut self, delegated: bool) {
        let _ = delegated;
    }
    fn invalidate(&mut self);
    fn dispose(&mut self);
}

pub struct EditorBorderContext {
    pub is_bash_mode: bool,
    pub thinking_level: String,
}

/// Mode-owned chrome seam for InteractiveMode: extensions own their editor factory, this strategy
/// only creates and decorates the base editor.
pub trait InteractiveChrome {
    fn tool_presentation(&self) -> &'static str;
    fn get_editor_theme(&self) -> EditorTheme;
    fn create_footer(&self, session: Rc<dyn GrokFooterSession>, theme: Theme) -> Box<dyn InteractiveFooter>;
    fn create_welcome_content(&self, app_name: &str, version: &str) -> Box<dyn Component>;
    fn create_working_indicator(
        &self,
        message: &str,
        indicator: Option<LoaderIndicatorOptions>,
    ) -> StatusIndicator;
    fn get_editor_border_color(&self, context: &EditorBorderContext) -> Rc<dyn Fn(&str) -> String>;
    fn arrange_root(&self, children: Vec<Rc<RefCell<dyn Component>>>, rows: usize) -> Vec<Rc<RefCell<dyn Component>>>;
}

struct GrokFooterSurface {
    content: Rc<RefCell<dyn Component>>,
    theme: Theme,
}

impl Component for GrokFooterSurface {
    fn invalidate(&mut self) {
        self.content.borrow_mut().invalidate();
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        let surface = get_grok_chrome_tokens(&self.theme).surface;
        self.content.borrow_mut().render(width).into_iter().map(|line| surface(&line)).collect()
    }
    fn dispose(&mut self) {
        self.content.borrow_mut().dispose();
    }
}

struct GrokRootSpacer {
    rows: usize,
    content: Vec<Rc<RefCell<dyn Component>>>,
}

impl Component for GrokRootSpacer {
    fn invalidate(&mut self) {}

    fn render(&mut self, width: usize) -> Vec<String> {
        let content_height: usize = self
            .content
            .iter()
            .map(|component| component.borrow_mut().render(width).len())
            .sum();
        vec![String::new(); self.rows.saturating_sub(content_height)]
    }
}

pub struct GrokEditor {
    card: GrokInputCard,
}

impl GrokEditor {
    pub fn new(base: Rc<RefCell<dyn Component>>, theme: Theme) -> Self {
        Self {
            card: GrokInputCard::new(base, theme),
        }
    }
}

impl Component for GrokEditor {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.card.render(width)
    }

    fn invalidate(&mut self) {
        self.card.invalidate();
    }

    fn dispose(&mut self) {
        self.card.dispose();
    }
}

pub struct GrokChrome {
    theme: Theme,
}

impl GrokChrome {
    pub fn new(theme: Theme) -> Self {
        Self { theme }
    }

    pub fn create_base_editor(&self, base: Rc<RefCell<dyn Component>>) -> GrokEditor {
        GrokEditor::new(base, self.theme.clone())
    }
}

impl InteractiveChrome for GrokChrome {
    fn tool_presentation(&self) -> &'static str {
        "grok"
    }

    fn get_editor_theme(&self) -> EditorTheme {
        let tokens = get_grok_chrome_tokens(&self.theme);
        let theme = self.theme.clone();
        let tokens2 = get_grok_chrome_tokens(&self.theme);
        let tokens3 = get_grok_chrome_tokens(&self.theme);
        let tokens4 = get_grok_chrome_tokens(&self.theme);
        let tokens5 = get_grok_chrome_tokens(&self.theme);
        EditorTheme {
            border_color: Rc::new(move |text: &str| (tokens.input_border)(text)),
            mention: None,
            select_list: maho_tui::components::select_list::SelectListTheme {
                selected_prefix: {
                    let theme = theme.clone();
                    Rc::new(move |text: &str| theme.fg(ThemeColor::Accent, text))
                },
                selected_text: Rc::new(move |text: &str| (tokens2.primary_text)(text)),
                description: Rc::new(move |text: &str| (tokens3.muted_text)(text)),
                scroll_info: Rc::new(move |text: &str| (tokens4.muted_text)(text)),
                no_match: Rc::new(move |text: &str| (tokens5.muted_text)(text)),
                render_row: None,
            },
        }
    }

    fn create_footer(&self, session: Rc<dyn GrokFooterSession>, theme: Theme) -> Box<dyn InteractiveFooter> {
        Box::new(GrokFooterHandle {
            footer: GrokFooter::new(session, theme),
        })
    }

    fn create_welcome_content(&self, app_name: &str, version: &str) -> Box<dyn Component> {
        Box::new(GrokWelcomeCard::new(app_name, version, self.theme.clone()))
    }

    fn create_working_indicator(
        &self,
        message: &str,
        indicator: Option<LoaderIndicatorOptions>,
    ) -> StatusIndicator {
        let theme = self.theme.clone();
        let mut indicator = indicator.unwrap_or_default();
        indicator.frames = Some(vec![glyphs::SPINNER.to_string()]);
        let theme = theme.clone();
        indicator.indicator_formatter = Some(Rc::new(move |frame: &str, _tick: u64| {
            theme.fg(ThemeColor::Accent, frame)
        }));
        StatusIndicator::new(
            crate::components::status_indicator::StatusIndicatorKind::Working,
            message,
            self.theme.clone(),
            Some(indicator),
            0,
        )
    }

    fn get_editor_border_color(&self, _context: &EditorBorderContext) -> Rc<dyn Fn(&str) -> String> {
        let tokens = get_grok_chrome_tokens(&self.theme);
        Rc::new(move |text: &str| (tokens.input_border)(text))
    }

    fn arrange_root(
        &self,
        children: Vec<Rc<RefCell<dyn Component>>>,
        rows: usize,
    ) -> Vec<Rc<RefCell<dyn Component>>> {
        let input_tail_start = children.len().saturating_sub(3);
        let content: Vec<Rc<RefCell<dyn Component>>> = children[..input_tail_start].to_vec();
        let input_tail: Vec<Rc<RefCell<dyn Component>>> = children[input_tail_start..].to_vec();
        let mut tail: Vec<Rc<RefCell<dyn Component>>> = Vec::new();
        if let Some((footer, rest)) = input_tail.split_last() {
            tail.extend(rest.iter().cloned());
            tail.push(Rc::new(RefCell::new(GrokFooterSurface {
                content: Rc::clone(footer),
                theme: self.theme.clone(),
            })));
        }
        let arranged: Vec<Rc<RefCell<dyn Component>>> =
            content.iter().cloned().chain(tail.iter().cloned()).collect();
        let mut result = content;
        result.push(Rc::new(RefCell::new(GrokRootSpacer {
            rows,
            content: arranged,
        })));
        result.extend(tail);
        result
    }
}

struct GrokFooterHandle {
    footer: GrokFooter,
}

impl Component for GrokFooterHandle {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.footer.render(width)
    }

    fn invalidate(&mut self) {
        self.footer.invalidate();
    }

    fn dispose(&mut self) {
        self.footer.dispose();
    }
}

impl InteractiveFooter for GrokFooterHandle {
    fn set_session(&mut self, session: Rc<dyn GrokFooterSession>) {
        self.footer.set_session(session);
    }

    fn set_auto_compact_enabled(&mut self, enabled: bool) {
        self.footer.set_auto_compact_enabled(enabled);
    }

    fn invalidate(&mut self) {
        self.footer.invalidate();
    }

    fn dispose(&mut self) {
        self.footer.dispose();
    }
}
