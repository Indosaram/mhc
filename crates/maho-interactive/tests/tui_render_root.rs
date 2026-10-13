//! Pins the fullscreen render-root mount contract.
//!
//! The alternate screen paints its `layoutRoot` (senpi `TuiAltScreen` `setLayoutRoot`) and, when
//! none is set, the base children through an implicit follow-end scroll view.
//! [`InteractiveTui::set_render_root`] dispatches to `addChild` for the main screen and
//! `setLayoutRoot` for the alt screen.

use std::cell::RefCell;
use std::rc::Rc;

use maho_interactive::theme::{ColorMode, Theme};
use maho_interactive::tui_renderer::{
    create_interactive_tui, InteractiveTui, InteractiveTuiOptions, TuiMode,
};
use maho_tui::components::text::Text;
use maho_tui::terminal::{InputHandler, ResizeHandler, Terminal, TerminalError};
use maho_tui::tui::Component;

struct RecordingTerminal {
    columns: u16,
    rows: u16,
    out: String,
}

impl Terminal for RecordingTerminal {
    fn start(&mut self, _on_input: InputHandler, _on_resize: ResizeHandler) {}
    fn stop(&mut self) -> Result<(), TerminalError> {
        Ok(())
    }
    fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
    fn write(&mut self, data: &str) {
        self.out.push_str(data);
    }
    fn columns(&self) -> u16 {
        self.columns
    }
    fn rows(&self) -> u16 {
        self.rows
    }
    fn kitty_protocol_active(&self) -> bool {
        true
    }
    fn move_by(&mut self, lines: i64) {
        if lines > 0 {
            self.write(&format!("\x1b[{lines}B"));
        } else if lines < 0 {
            self.write(&format!("\x1b[{}A", -lines));
        }
    }
    fn hide_cursor(&mut self) {
        self.write("\x1b[?25l");
    }
    fn show_cursor(&mut self) {
        self.write("\x1b[?25h");
    }
    fn clear_line(&mut self) {
        self.write("\x1b[K");
    }
    fn clear_from_cursor(&mut self) {
        self.write("\x1b[J");
    }
    fn clear_screen(&mut self) {
        self.write("\x1b[2J\x1b[H");
    }
    fn set_title(&mut self, title: &str) {
        self.write(&format!("\x1b]0;{title}\x07"));
    }
    fn set_progress(&mut self, _active: bool) {}
}

fn fullscreen() -> InteractiveTui {
    let theme = Theme::builtin("dark", ColorMode::Truecolor).expect("dark theme");
    create_interactive_tui(
        InteractiveTuiOptions {
            tui_mode: TuiMode::Fullscreen,
            show_hardware_cursor: false,
            bottom_shortcut: String::new(),
        },
        theme,
    )
}

fn painted(mount: impl FnOnce(&mut InteractiveTui, Rc<RefCell<dyn Component>>)) -> String {
    let root: Rc<RefCell<dyn Component>> =
        Rc::new(RefCell::new(Text::with_padding("mounted-root", 0, 0)));
    let mut tui = fullscreen();
    let mut terminal = RecordingTerminal {
        columns: 80,
        rows: 24,
        out: String::new(),
    };
    tui.before_terminal_start(&mut terminal, false, false);
    mount(&mut tui, root);
    tui.do_render(&mut terminal);
    terminal.out
}

#[test]
fn set_render_root_paints_the_fullscreen_alt_screen() {
    let out = painted(|tui, root| tui.set_render_root(root));
    assert!(
        out.contains("mounted-root"),
        "set_render_root must mount the alt screen's layoutRoot so fullscreen paints"
    );
}

#[test]
fn child_only_mount_paints_through_the_implicit_scroll_view() {
    // senpi `TuiAltScreen.doRender` renders `layoutRoot ?? implicitScrollView`, and the implicit
    // scroll view wraps the base children, so a child-only mount still paints in fullscreen.
    let out = painted(|tui, root| tui.base_mut().add_child(root));
    assert!(
        out.contains("mounted-root"),
        "a child-only mount must paint through the implicit scroll view"
    );
}
