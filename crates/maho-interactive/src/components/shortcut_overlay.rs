use maho_tui::{components::text::Text, tui::Component, utils::visible_width};
use crate::theme::{Theme, ThemeColor};
use super::{keybinding_hints::{key_hint, raw_key_hint}, theme_selector::dynamic_border};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorInputKind { Typed, Paste, Other }

pub fn should_show_shortcut_overlay(previous: &str, next: &str, kind: EditorInputKind) -> bool {
    previous.is_empty() && next == "?" && kind == EditorInputKind::Typed
}

pub fn classify_editor_input(previous: &str, next: &str, paste_signalled: bool) -> EditorInputKind {
    if paste_signalled || next.encode_utf16().count().saturating_sub(previous.encode_utf16().count()) > 1 { EditorInputKind::Paste } else { EditorInputKind::Typed }
}

pub struct ShortcutOverlay { theme: Theme }
impl ShortcutOverlay { pub fn new(theme: &Theme) -> Self { Self { theme: theme.clone() } } }

impl Component for ShortcutOverlay {
    fn render(&mut self, width: usize) -> Vec<String> {
        let rows = [
            (key_hint("app.interrupt", "interrupt", &self.theme), key_hint("app.clear", "clear editor", &self.theme)),
            (key_hint("app.exit", "exit", &self.theme), key_hint("app.thinking.cycle", "thinking level", &self.theme)),
            (key_hint("app.model.cycleForward", "next model", &self.theme), key_hint("app.model.select", "select model", &self.theme)),
            (key_hint("app.tools.expand", "expand tools", &self.theme), key_hint("app.editor.external", "external editor", &self.theme)),
            (key_hint("app.message.followUp", "queue follow-up", &self.theme), key_hint("app.history.search", "search history", &self.theme)),
            (raw_key_hint("!", "bash", &self.theme), raw_key_hint("/", "commands", &self.theme)),
        ];
        let column = rows.iter().map(|(left, _)| visible_width(left)).max().unwrap_or(0);
        let grid = rows.iter().map(|(left, right)| format!("{left}{}{right}", " ".repeat(column - visible_width(left) + 4))).collect::<Vec<_>>().join("\n");
        let mut lines = vec![dynamic_border(&self.theme, ThemeColor::Accent, width)];
        lines.extend(Text::with_padding(self.theme.fg(ThemeColor::Accent, &self.theme.bold(" Keyboard shortcuts")), 0, 0).render(width));
        lines.extend(Text::with_padding(grid, 1, 0).render(width));
        lines.extend(Text::with_padding(self.theme.fg(ThemeColor::Dim, " /help for the full reference · any key to dismiss"), 0, 0).render(width));
        lines.push(dynamic_border(&self.theme, ThemeColor::Accent, width));
        lines
    }
}
