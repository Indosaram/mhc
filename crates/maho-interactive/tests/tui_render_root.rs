//! Pins the fullscreen render-root mount contract.
//!
//! The alternate screen paints only its own `layoutRoot` (senpi `TuiAltScreen` `setLayoutRoot`); a
//! root mounted as a plain base child is never read there. [`InteractiveTui::set_render_root`]
//! dispatches to `addChild` for the main screen and `setLayoutRoot` for the alt screen, so
//! `--tui-mode fullscreen` cannot silently go blank the way it did when the entry called
//! `base.add_child` for every mode.

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
fn child_only_mount_leaves_the_fullscreen_alt_screen_blank() {
    // Guards the exact defect: `base.add_child` fills the base container the alt screen never
    // reads, leaving `layoutRoot` unset so `do_render` paints nothing.
    let out = painted(|tui, root| tui.base_mut().add_child(root));
    assert!(
        !out.contains("mounted-root"),
        "a child-only mount must not paint in fullscreen"
    );
}
