use std::{cell::RefCell, rc::Rc};
use maho_tui::{components::box_::Box as NoticeBox, tui::Component, utils::wrap_text_with_ansi};
use crate::rules::types::{MatchReason, RuleDiagnostic};

pub struct BannerRule {
    pub relative_path: String,
    pub match_reason: MatchReason,
}

pub struct RulesBannerProps {
    pub rule_count: usize,
    pub diagnostics: Vec<RuleDiagnostic>,
    pub top_rules: Vec<BannerRule>,
}

/// Coloring is supplied by the UI owner, like DynamicBorder's color callback.
pub type Foreground = Rc<dyn Fn(&str, &str) -> String>;
pub type Background = Rc<dyn Fn(&str) -> String>;

pub struct RulesBanner {
    props: RulesBannerProps,
    foreground: Foreground,
    background: Background,
}

impl RulesBanner {
    pub fn new(props: RulesBannerProps, foreground: Foreground, background: Background) -> Self {
        Self { props, foreground, background }
    }
}

struct NoticeText(String);
impl Component for NoticeText {
    fn render(&mut self, width: usize) -> Vec<String> {
        wrap_text_with_ansi(&self.0, width.max(1))
    }
    fn invalidate(&mut self) {}
}

impl Component for RulesBanner {
    fn render(&mut self, width: usize) -> Vec<String> {
        render_banner_lines(&self.props, &self.foreground, &self.background, width)
    }
    fn invalidate(&mut self) {}
}

pub fn render_banner_lines(props: &RulesBannerProps, foreground: &Foreground, background: &Background, width: usize) -> Vec<String> {
    let mut notice = NoticeBox::with_padding(1, 1);
    notice.set_bg_fn(Some(Rc::clone(background)));
    let (title, why) = if props.rule_count == 0 {
        ("[pi-rules] No rules discovered".into(), "No rules were discovered.".into())
    } else {
        (format!("[pi-rules] {} active rules", props.rule_count), format!("{} active rules were discovered.", props.rule_count))
    };
    let mut lines = vec![("accent", format!("\x1b[1m{title}\x1b[22m")), ("dim", why)];
    if props.rule_count > 0 {
        for rule in &props.top_rules {
            let has_diagnostic = props.diagnostics.iter().any(|diagnostic| diagnostic.source == rule.relative_path);
            let annotation = match &rule.match_reason {
                MatchReason::Glob { pattern } => format!(" {pattern}"),
                MatchReason::AlwaysApply | MatchReason::SingleFile | MatchReason::NoMatch => String::new(),
            };
            lines.push((if has_diagnostic { "error" } else { "success" }, format!("  {} {}{annotation}", if has_diagnostic { "⚠" } else { "●" }, rule.relative_path)));
        }
        if !props.diagnostics.is_empty() {
            lines.push(("warning", format!("  ⚠ {} warning(s)", props.diagnostics.len())));
        }
    }
    for (tone, text) in lines {
        notice.add_child(Rc::new(RefCell::new(NoticeText(foreground(tone, &text)))));
    }
    notice.render(width)
}

pub struct StatusLineInput {
    pub rule_count: usize,
    pub has_errors: bool,
}

pub fn status_line_text(input: &StatusLineInput, foreground: &Foreground) -> String {
    let base = format!("[pi-rules] {} active", input.rule_count);
    if input.has_errors {
        format!("{}{}", foreground("muted", &format!("{base} · ")), foreground("error", "⚠ errors"))
    } else {
        foreground("muted", &base)
    }
}
