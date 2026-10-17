//! senpi `TuiBase.start`/`stop`, terminal protocol reply consumption, terminal color queries, and
//! the alt-screen focus-report branch of `handleViewportInput`.

use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::components::text::Text;
use maho_tui::process_env::with_overrides;
use maho_tui::terminal::{InputHandler, ResizeHandler, Terminal, TerminalError};
use maho_tui::terminal_colors::{RgbColor, TerminalColorScheme};
use maho_tui::tui::{Component, TuiBase};
use maho_tui::tui_alt_screen::TuiAltScreen;

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

struct Recorder(Rc<RefCell<Vec<String>>>);
impl Component for Recorder {
    fn render(&mut self, _width: usize) -> Vec<String> {
        Vec::new()
    }
    fn handle_input(&mut self, data: &str) {
        self.0.borrow_mut().push(data.to_owned());
    }
    fn has_input_handler(&self) -> bool {
        true
    }
}

fn focused_recorder(base: &mut TuiBase) -> Rc<RefCell<Vec<String>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let component: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(Recorder(seen.clone())));
    base.set_focus(Some(component));
    seen
}

#[test]
fn start_hides_cursor_and_enables_tmux_focus_reporting_only_under_tmux() {
    with_overrides(&[("TMUX", Some("/tmp/tmux-1/default,1,0"))], || {
        let mut base = TuiBase::new();
        let mut terminal = RecordingTerminal::default();
        base.prepare_start();
        base.after_terminal_start(&mut terminal);
        assert!(terminal.out.starts_with("\x1b[?1004h\x1b[?25l"), "{:?}", terminal.out);
    });
    with_overrides(&[("TMUX", None)], || {
        let mut base = TuiBase::new();
        let mut terminal = RecordingTerminal::default();
        base.prepare_start();
        base.after_terminal_start(&mut terminal);
        assert!(!terminal.out.contains("\x1b[?1004h"));
        assert!(terminal.out.starts_with("\x1b[?25l"));
    });
}

#[test]
fn stop_parks_cursor_below_the_last_frame_and_shows_it() {
    with_overrides(&[("TMUX", None)], || {
        let mut base = TuiBase::new();
        let mut terminal = RecordingTerminal::default();
        {
            let mut state = base.state.borrow_mut();
            state.previous_lines = vec!["a".into(), "b".into(), "c".into()];
            state.hardware_cursor_row = 1;
        }
        base.begin_stop(&mut terminal);
        base.finish_stop_output(&mut terminal, false);
        assert_eq!(terminal.out, "\x1b[2B\r\n\x1b[?25h");
    });
}

#[test]
fn stop_with_preserved_screen_only_shows_the_cursor() {
    with_overrides(&[("TMUX", None)], || {
        let mut base = TuiBase::new();
        let mut terminal = RecordingTerminal::default();
        base.state.borrow_mut().previous_lines = vec!["a".into()];
        base.begin_stop(&mut terminal);
        base.finish_stop_output(&mut terminal, true);
        assert_eq!(terminal.out, "\x1b[?25h");
    });
}

#[test]
fn cell_size_reply_updates_cell_dimensions_without_reaching_focus() {
    let previous = maho_tui::terminal_image::get_cell_dimensions();
    let mut base = TuiBase::new();
    let seen = focused_recorder(&mut base);
    base.handle_terminal_input("\x1b[6;20;10t", false);
    let dimensions = maho_tui::terminal_image::get_cell_dimensions();
    maho_tui::terminal_image::set_cell_dimensions(previous);
    assert_eq!((dimensions.width_px, dimensions.height_px), (10, 20));
    assert!(seen.borrow().is_empty());
}

#[test]
fn osc11_reply_settles_the_background_query_and_is_consumed() {
    let mut base = TuiBase::new();
    let mut terminal = RecordingTerminal::default();
    let seen = focused_recorder(&mut base);
    let ticket = base.query_terminal_colors(&mut terminal, 100, 0);
    assert!(terminal.out.starts_with("\x1b]10;?\x07\x1b]11;?\x07"));
    assert!(terminal.out.ends_with("\x1b[c"));
    assert!(ticket.result().is_none());
    base.handle_terminal_input("\x1b]11;rgb:ffff/ffff/ffff\x07", false);
    assert!(ticket.result().is_none());
    base.handle_terminal_input("\x1b[?62;22c", false);
    assert_eq!(ticket.result().and_then(|colors| colors.background), Some(RgbColor { r: 255, g: 255, b: 255 }));
    assert!(seen.borrow().is_empty());
}

#[test]
fn osc11_reply_without_an_outstanding_query_reaches_focus() {
    let mut base = TuiBase::new();
    let seen = focused_recorder(&mut base);
    base.handle_terminal_input("\x1b]11;rgb:ffff/ffff/ffff\x07", false);
    assert_eq!(seen.borrow().len(), 1);
}

#[test]
fn terminal_query_settles_empty_after_its_timeout() {
    let mut base = TuiBase::new();
    let mut terminal = RecordingTerminal::default();
    let ticket = base.query_terminal_color_scheme(&mut terminal, 100, 0);
    assert_eq!(terminal.out, "\x1b[?996n");
    base.poll_terminal_queries(99);
    assert_eq!(ticket.result(), None);
    base.poll_terminal_queries(100);
    assert_eq!(ticket.result(), Some(None));
}

#[test]
fn color_scheme_report_settles_query_and_notifies_listeners() {
    let mut base = TuiBase::new();
    let mut terminal = RecordingTerminal::default();
    let reports = Rc::new(RefCell::new(Vec::new()));
    let captured = reports.clone();
    base.on_terminal_color_scheme_change(Box::new(move |scheme| captured.borrow_mut().push(scheme)));
    let ticket = base.query_terminal_color_scheme(&mut terminal, 100, 0);
    let seen = focused_recorder(&mut base);
    base.handle_terminal_input("\x1b[?997;2n", false);
    assert_eq!(ticket.result(), Some(Some(TerminalColorScheme::Light)));
    assert_eq!(*reports.borrow(), [TerminalColorScheme::Light]);
    assert!(seen.borrow().is_empty());
}

#[test]
fn color_scheme_notifications_toggle_only_while_running() {
    let mut base = TuiBase::new();
    let mut terminal = RecordingTerminal::default();
    base.set_terminal_color_scheme_notifications(true, &mut terminal);
    assert_eq!(terminal.out, "\x1b[?2031h");
    base.begin_stop(&mut terminal);
    assert_eq!(terminal.out, "\x1b[?2031h\x1b[?2031l");
    base.set_terminal_color_scheme_notifications(false, &mut terminal);
    assert_eq!(terminal.out, "\x1b[?2031h\x1b[?2031l");
}

#[test]
fn main_screen_consumes_tmux_focus_events() {
    let mut base = TuiBase::new();
    let seen = focused_recorder(&mut base);
    base.handle_terminal_input("\x1b[I", false);
    base.handle_terminal_input("\x1b[Ox", false);
    assert_eq!(*seen.borrow(), ["x"]);
}

#[test]
fn alt_screen_consumes_focus_reports_before_the_editor() {
    let mut alt = TuiAltScreen::new();
    let root: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(Text::with_padding("doc", 0, 0)));
    alt.set_layout_root(Some(root));
    let seen = focused_recorder(&mut alt.base);
    alt.handle_terminal_input("\x1b[I", false);
    alt.handle_terminal_input("\x1b[O", false);
    assert!(seen.borrow().is_empty());
    alt.handle_terminal_input("k", false);
    assert_eq!(*seen.borrow(), ["k"]);
}

#[test]
fn alt_screen_focus_out_drops_an_active_drag_selection() {
    let mut alt = TuiAltScreen::new();
    let root: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(Text::with_padding("hello world", 0, 0)));
    alt.set_layout_root(Some(root));
    let mut terminal = RecordingTerminal::default();
    alt.before_terminal_start(&mut terminal, true, false);
    alt.do_render(&mut terminal);
    assert!(alt.handle_mouse_input("\x1b[<0;1;1M", 0, &mut terminal));
    assert!(alt.handle_mouse_input("\x1b[<32;6;1M", 1, &mut terminal));
    assert!(alt.has_active_selection());
    assert!(alt.handle_mouse_input("\x1b[O", 2, &mut terminal));
    assert!(!alt.has_active_selection());
}
