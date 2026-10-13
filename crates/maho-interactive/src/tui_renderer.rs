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
            Self::Regular(tui) => { let _ = tui.take_mouse_external_write_pending(terminal); tui.base.do_render(terminal); tui.note_render(terminal); }
            Self::Fullscreen(tui) => tui.do_render(terminal),
        }
    }

    /// Per-iteration poll of senpi's timer/promise callbacks: the main screen's in-flight CPR
    /// mouse-anchor query, and the alternate screen's flash expiry and scrollbar hide timers.
    pub fn poll_timers(&mut self, now_ms: u64) {
        match self {
            Self::Regular(tui) => tui.base.poll_mouse_anchor_query(),
            Self::Fullscreen(tui) => tui.poll_timers(now_ms),
        }
    }

    /// senpi `ui.flash(message)` when the UI is a `TuiAltScreen`; returns false on the main screen.
    pub fn flash(&mut self, message: &str, now_ms: u64) -> bool {
        match self {
            Self::Regular(_) => false,
            Self::Fullscreen(tui) => { tui.flash(message, None, now_ms); true }
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

    /// senpi's `TuiBase.acquireMouseCapture(reason)`: takes a lease and, for the first lease,
    /// applies tracking through the renderer's `applyMouseTracking`. Only the main screen
    /// overrides `applyMouseTracking`; the alternate screen keeps the base no-op because its
    /// tracking is owned by `beforeTerminalStart`. Returns the token for
    /// [`InteractiveTui::release_mouse_capture`].
    pub fn acquire_mouse_capture(&mut self, reason: &str, terminal: &mut dyn maho_tui::terminal::Terminal) -> u64 {
        match self {
            Self::Regular(tui) => {
                let (token, should_apply) = tui.base.acquire_mouse_capture(reason);
                if should_apply {
                    let enabled = tui.base.mouse_capture_enabled();
                    tui.apply_mouse_tracking(enabled, &main_screen_mouse_support(), terminal);
                    tui.base.calibrate_mouse_anchor(terminal);
                }
                token
            }
            Self::Fullscreen(tui) => tui.base.acquire_mouse_capture(reason).0,
        }
    }

    /// The release closure returned by senpi's `acquireMouseCapture`.
    pub fn release_mouse_capture(&mut self, token: u64, terminal: &mut dyn maho_tui::terminal::Terminal) {
        match self {
            Self::Regular(tui) => {
                if tui.base.release_mouse_capture(token) {
                    tui.apply_mouse_tracking(false, &main_screen_mouse_support(), terminal);
                }
            }
            Self::Fullscreen(tui) => {
                tui.base.release_mouse_capture(token);
            }
        }
    }

    pub fn is_regular(&self) -> bool {
        matches!(self, Self::Regular(_))
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
/// senpi `TuiMainScreen.applyMouseTracking`'s support predicate: a TTY stdout, not Termux, and on
/// Windows only inside Windows Terminal.
fn main_screen_mouse_support() -> maho_tui::tui_main_screen::MouseTrackingSupport {
    use std::io::IsTerminal;
    maho_tui::tui_main_screen::MouseTrackingSupport {
        is_tty: std::io::stdout().is_terminal(),
        is_termux: std::env::var_os("TERMUX_VERSION").is_some_and(|value| !value.is_empty()),
        is_windows_without_wt: cfg!(windows) && std::env::var_os("WT_SESSION").is_none_or(|value| value.is_empty()),
    }
}

/// senpi `createInteractiveTui`'s default terminal: a `ProcessTerminal` whose stray stdout writes
/// are diverted to the hidden-stdout debug log and whose stderr observation follows the visible
/// stderr sink, so external writes cannot desynchronize the differential renderer.
pub fn create_process_terminal() -> maho_tui::terminal::ProcessTerminal {
    use std::sync::Arc;
    maho_tui::terminal::ProcessTerminal::new(maho_tui::terminal::ProcessTerminalOptions {
        on_external_stdout_write: Some(Arc::new(|text: &str| {
            maho_core::hidden_stdout_log::append_hidden_tui_stdout(text);
            Ok(())
        })),
        observe_external_stderr_writes: Some(Arc::new(|listener: maho_tui::terminal::ExternalWriteListener| {
            let observation = maho_core::output_guard::observe_visible_stderr_writes(listener);
            Box::new(move || drop(observation)) as Box<dyn FnOnce() + Send>
        })),
        ..Default::default()
    })
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
