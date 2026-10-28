//! Main-screen scrollback handoff regression tests.
use maho_tui::mouse_input::MouseTracking;
use maho_tui::terminal::{InputHandler, ResizeHandler, Terminal, TerminalError};
use maho_tui::tui_main_screen::{MouseTrackingSupport, TuiMainScreen};
#[derive(Default)]
struct RecordingTerminal {
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
        80
    }
    fn rows(&self) -> u16 {
        24
    }
    fn kitty_protocol_active(&self) -> bool {
        true
    }
    fn move_by(&mut self, _lines: i64) {}
    fn hide_cursor(&mut self) {
        self.write("\x1b[?25l");
    }
    fn show_cursor(&mut self) {
        self.write("\x1b[?25h");
    }
    fn clear_line(&mut self) {}
    fn clear_from_cursor(&mut self) {}
    fn clear_screen(&mut self) {}
    fn set_title(&mut self, _title: &str) {}
    fn set_progress(&mut self, _active: bool) {}
}

#[test]
fn first_wheel_yields_and_only_key_press_rearms() {
    let mut screen = TuiMainScreen::new();
    let mut terminal = RecordingTerminal::default();
    screen.base.state.borrow_mut().stopped = false;
    let support = MouseTrackingSupport {
        is_tty: true,
        is_termux: false,
        is_windows_without_wt: false,
    };
    screen.acquire_mouse_capture("test", &support, &mut terminal);
    assert_eq!(screen.mouse_capture_state(), "armed");
    assert!(screen.handle_mouse_input("\x1b[<64;1;1M", 0, &mut terminal));
    assert_eq!(screen.mouse_capture_state(), "yielded");
    assert!(terminal.out.ends_with(MouseTracking::DISABLE));
    screen.handle_mouse_input("\x1b[?1;2c", 0, &mut terminal);
    screen.handle_mouse_input("\x1b[4;10;20t", 0, &mut terminal);
    screen.handle_mouse_input("\x1b[97;1:3u", 0, &mut terminal);
    assert_eq!(screen.mouse_capture_state(), "yielded");
    assert!(!screen.handle_mouse_input("a", 0, &mut terminal));
    assert_eq!(screen.mouse_capture_state(), "armed");
    assert!(terminal.out.ends_with(MouseTracking::INLINE));
}
#[test]
fn released_capture_does_not_rearm_after_yield() {
    let mut screen = TuiMainScreen::new();
    let mut terminal = RecordingTerminal::default();
    screen.base.state.borrow_mut().stopped = false;
    let support = MouseTrackingSupport {
        is_tty: true,
        is_termux: false,
        is_windows_without_wt: false,
    };
    screen.acquire_mouse_capture("test", &support, &mut terminal);
    screen.handle_mouse_input("\x1b[<65;1;1M", 0, &mut terminal);
    screen.release_mouse_capture(&support, &mut terminal);
    screen.handle_mouse_input("a", 0, &mut terminal);
    assert_eq!(screen.mouse_capture_state(), "off");
}
