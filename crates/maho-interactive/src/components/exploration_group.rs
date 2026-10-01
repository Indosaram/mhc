//! Port of `components/exploration-group.ts`.
use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::tui::{Component, Container, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType};
use maho_tui::utils::truncate_to_width;

use super::exploration_call::{ExplorationAction, ExplorationCall};
use super::tool_execution::ToolExecutionComponent;
use crate::theme::{Theme, ThemeColor};
use crate::tool_progress::tool_spinner_glyph;

const MAX_BODY_LINES: usize = 8;
const SPINNER_FRAME_MS: f64 = 80.;

fn body_lines(calls: &[ExplorationCall], theme: &Theme) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut index = 0;
    while index < calls.len() {
        let first = &calls[index];
        index += 1;
        if first.action == ExplorationAction::Read && !first.failed {
            let mut names: Vec<String> = vec![first.label.clone()];
            while index < calls.len() && calls[index].action == ExplorationAction::Read && !calls[index].failed {
                names.push(calls[index].label.clone());
                index += 1;
            }
            let mut unique: Vec<String> = Vec::new();
            for name in names {
                if !name.is_empty() && !unique.contains(&name) {
                    unique.push(name);
                }
            }
            lines.push(
                format!("{} {}", theme.fg(ThemeColor::Accent, "Read"), unique.join(&theme.fg(ThemeColor::Dim, ", ")))
                    .trim_end()
                    .to_owned(),
            );
            continue;
        }
        let failed = if first.failed { theme.fg(ThemeColor::Error, " (failed)") } else { String::new() };
        let action = match first.action {
            ExplorationAction::Read => "Read",
            ExplorationAction::Search => "Search",
            ExplorationAction::List => "List",
        };
        lines.push(format!("{} {}", theme.fg(ThemeColor::Accent, action), first.label).trim_end().to_owned() + &failed);
    }
    lines
}

pub struct ExplorationGroup {
    children: Container,
    calls: Vec<(Rc<RefCell<ToolExecutionComponent>>, ExplorationCall)>,
    rules: Vec<String>,
    theme: Theme,
    now_ms: f64,
}

impl ExplorationGroup {
    pub fn new(theme: Theme) -> Self {
        Self { children: Container::new(), calls: Vec::new(), rules: Vec::new(), theme, now_ms: 0. }
    }

    pub fn set_now_ms(&mut self, now_ms: f64) {
        self.now_ms = now_ms;
    }

    pub fn set_members(
        &mut self,
        members: Vec<Rc<RefCell<dyn Component>>>,
        calls: Vec<(Rc<RefCell<ToolExecutionComponent>>, ExplorationCall)>,
        rules: Vec<String>,
    ) {
        self.children.children = members;
        self.calls = calls;
        self.rules = rules;
    }

    pub fn calls(&self) -> &[(Rc<RefCell<ToolExecutionComponent>>, ExplorationCall)] {
        &self.calls
    }

    fn expanded(&self) -> bool {
        self.calls.iter().any(|(component, _)| component.borrow().is_expanded())
    }
}

impl Component for ExplorationGroup {
    fn render(&mut self, width: usize) -> Vec<String> {
        let pending = self.calls.iter().any(|(_, call)| call.pending);
        let failed = self.calls.iter().filter(|(_, call)| call.failed).count();
        let marker = if pending {
            self.theme.fg(
                ThemeColor::Accent,
                tool_spinner_glyph((self.now_ms / SPINNER_FRAME_MS) as i64),
            )
        } else {
            self.theme.fg(ThemeColor::Dim, "•")
        };
        let title = if pending { "Exploring" } else { "Explored" };
        let header = format!(
            "{marker} {}{}",
            self.theme.bold(title),
            if failed > 0 { self.theme.fg(ThemeColor::Error, &format!(" · {failed} failed")) } else { String::new() }
        );
        let mut lines = vec![String::new(), truncate_to_width(&header, width, "...", false)];
        if self.expanded() {
            lines.extend(self.children.render(width));
            return lines;
        }
        let mut body = body_lines(&self.calls.iter().map(|(_, call)| call.clone()).collect::<Vec<_>>(), &self.theme);
        let mut unique_rules: Vec<&String> = Vec::new();
        for rule in &self.rules {
            if !unique_rules.contains(&rule) {
                unique_rules.push(rule);
            }
        }
        let rule_count = unique_rules.len();
        if rule_count > 0 {
            body.push(format!(
                "{} {rule_count} project {}",
                self.theme.fg(ThemeColor::Accent, "Applied"),
                if rule_count == 1 { "rule" } else { "rules" }
            ));
        }
        let shown: Vec<String> = body.iter().take(MAX_BODY_LINES).cloned().collect();
        if body.len() > shown.len() {
            let mut shown = shown;
            shown.push(self.theme.fg(ThemeColor::Dim, &format!("… +{} more", body.len() - shown.len())));
            for (index, line) in shown.iter().enumerate() {
                let prefix = if index == 0 { self.theme.fg(ThemeColor::Dim, "  └ ") } else { String::from("    ") };
                lines.push(truncate_to_width(&format!("{prefix}{line}"), width, "...", false));
            }
            return lines;
        }
        for (index, line) in shown.iter().enumerate() {
            let prefix = if index == 0 { self.theme.fg(ThemeColor::Dim, "  └ ") } else { String::from("    ") };
            lines.push(truncate_to_width(&format!("{prefix}{line}"), width, "...", false));
        }
        lines
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if (event.event_type == TuiMouseEventType::Press || event.event_type == TuiMouseEventType::Click)
            && event.button == TuiMouseButton::Left
            && (event.y == 1 || !self.expanded())
        {
            if event.event_type == TuiMouseEventType::Click {
                let expanded = !self.expanded();
                for (component, _) in &self.calls {
                    component.borrow_mut().set_expanded(expanded);
                }
            }
            return Some(TuiMouseEventResult { handled: true, ..Default::default() });
        }
        if self.expanded() && event.y >= 2 {
            let sub_event = TuiMouseEvent { y: event.y - 2, height: event.height.saturating_sub(2), ..*event };
            return self.children.handle_mouse(&sub_event);
        }
        None
    }

    fn invalidate(&mut self) {
        self.children.invalidate();
    }

    fn dispose(&mut self) {
        self.children.dispose();
    }
}
