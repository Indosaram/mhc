//! Port of tui-renderer.ts. Terminal IO is supplied by the host render loop.
use crate::theme::{Theme, ThemeBg, ThemeColor};
use maho_tui::{
    tui::{Component, TuiBase},
    tui_alt_screen::TuiAltScreen,
    tui_main_screen::TuiMainScreen,
};
use std::{cell::RefCell, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuiMode {
    Regular,
    Fullscreen,
}
pub struct InteractiveTuiOptions {
    pub tui_mode: TuiMode,
    pub show_hardware_cursor: bool,
    pub bottom_shortcut: String,
}
pub enum InteractiveTui {
    Regular(Box<TuiMainScreen>),
    Fullscreen(Box<TuiAltScreen>),
}
impl InteractiveTui {
    pub fn handle_mouse_input(&mut self, data: &str, now_ms: i64, terminal: &mut dyn maho_tui::terminal::Terminal) -> bool {
        match self {
            Self::Regular(tui) => tui.handle_mouse_input(data, now_ms, terminal),
            Self::Fullscreen(tui) => tui.handle_mouse_input(data, now_ms, terminal),
        }
    }

    pub fn do_render(&mut self, terminal: &mut dyn maho_tui::terminal::Terminal) {
        match self {
            Self::Regular(tui) => { tui.base.do_render(terminal); tui.note_render(terminal); }
            Self::Fullscreen(tui) => tui.do_render(terminal),
        }
    }

    pub fn before_terminal_start(&mut self, terminal: &mut dyn maho_tui::terminal::Terminal, mouse_enabled: bool, multiplexer: bool) {
        if let Self::Fullscreen(tui) = self { tui.before_terminal_start(terminal, mouse_enabled, multiplexer); }
    }

    pub fn before_terminal_stop(&mut self, terminal: &mut dyn maho_tui::terminal::Terminal, mouse_enabled: bool) {
        match self {
            Self::Regular(tui) => tui.before_terminal_stop(terminal),
            Self::Fullscreen(tui) => tui.before_terminal_stop(terminal, mouse_enabled),
        }
    }

    pub fn after_terminal_stop(&mut self, terminal: &mut dyn maho_tui::terminal::Terminal, preserve_screen: bool) {
        if let Self::Fullscreen(tui) = self { tui.after_terminal_stop(terminal, preserve_screen); }
    }

    pub fn base(&self) -> &TuiBase {
        match self {
            Self::Regular(t) => &t.base,
            Self::Fullscreen(t) => &t.base,
        }
    }
    pub fn base_mut(&mut self) -> &mut TuiBase {
        match self {
            Self::Regular(t) => &mut t.base,
            Self::Fullscreen(t) => &mut t.base,
        }
    }

    /// Mounts the app's single render root on the renderer that reads it. senpi's main screen
    /// renders its mounted children (`addChild`), while the alternate screen renders only its
    /// `layoutRoot` (`setLayoutRoot`); a root added as a plain child is invisible in fullscreen, so
    /// every entry mounts through here (see `InteractiveTerminal::new`).
    pub fn set_render_root(&mut self, root: Rc<RefCell<dyn Component>>) {
        match self {
            Self::Regular(tui) => tui.base.add_child(root),
            Self::Fullscreen(tui) => tui.set_layout_root(Some(root)),
        }
    }
}
pub fn create_interactive_tui(options: InteractiveTuiOptions, theme: Theme) -> InteractiveTui {
    let mut tui = match options.tui_mode {
        TuiMode::Regular => InteractiveTui::Regular(Box::default()),
        TuiMode::Fullscreen => {
            let mut t = TuiAltScreen::new();
            t.set_scroll_to_end_indicator(Some(Rc::new(move || {
                let shortcut = if options.bottom_shortcut.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", options.bottom_shortcut)
                };
                theme.bg(
                    ThemeBg::SelectedBg,
                    &theme.fg(
                        ThemeColor::Text,
                        &format!(" ↓ Jump to latest message{shortcut} "),
                    ),
                )
            })));
            InteractiveTui::Fullscreen(Box::new(t))
        }
    };
    tui.base_mut()
        .set_show_hardware_cursor(options.show_hardware_cursor);
    tui
}
/// Resolve the current renderer for every call, including after replacement.
#[derive(Clone)]
pub struct InteractiveTuiReference {
    current: Rc<RefCell<InteractiveTui>>,
}
impl InteractiveTuiReference {
    pub fn new(current: Rc<RefCell<InteractiveTui>>) -> Self {
        Self { current }
    }
    pub fn with<R>(&self, f: impl FnOnce(&mut InteractiveTui) -> R) -> R {
        f(&mut self.current.borrow_mut())
    }
    pub fn replace(&self, tui: InteractiveTui) {
        *self.current.borrow_mut() = tui;
    }
}
