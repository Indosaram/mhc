//! Byte-exact full-screen parity for the main and alternate screen renderers.
//!
//! Each `tools/golden/cases/tui-screen-*.json` case is replayed against the Rust renderer. The
//! `.ansi` fixture is the byte stream senpi's renderer wrote for the same steps and the
//! `.<cols>x<rows>.json` fixture is the screen senpi's VirtualTerminal shows afterwards.

use std::cell::RefCell;
use std::rc::Rc;

use maho_test_support::vterm::VirtualTerminal;
use maho_tui::components::text::Text;
use maho_tui::terminal::{InputHandler, ResizeHandler, Terminal, TerminalError};
use maho_tui::tui::Component;
use maho_tui::tui_alt_screen::TuiAltScreen;
use maho_tui::tui_main_screen::TuiMainScreen;
use serde_json::Value;

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

fn case_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tools")
        .join("golden")
        .join("cases")
        .join(format!("{name}.json"))
}

fn fixture_path(file: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(file)
}

fn text_component(step: &Value) -> Rc<RefCell<dyn Component>> {
    let text = step["text"].as_str().unwrap_or_default();
    let padding_x = step["paddingX"].as_u64().unwrap_or(1) as usize;
    let padding_y = step["paddingY"].as_u64().unwrap_or(1) as usize;
    Rc::new(RefCell::new(Text::with_padding(text, padding_x, padding_y)))
}

fn replay(name: &str) -> (String, u16, u16) {
    let spec: Value = serde_json::from_str(
        &std::fs::read_to_string(case_path(name)).unwrap_or_else(|error| panic!("{name}: {error}")),
    )
    .unwrap_or_else(|error| panic!("{name}: {error}"));
    let mode = spec["mode"].as_str().expect("mode").to_string();
    let mut terminal = RecordingTerminal {
        columns: spec["cols"].as_u64().expect("cols") as u16,
        rows: spec["rows"].as_u64().expect("rows") as u16,
        out: String::new(),
    };

    let mut main_screen = TuiMainScreen::new();
    let mut alt_screen = TuiAltScreen::new();
    if mode == "alt" {
        alt_screen.before_terminal_start(&mut terminal, true, false);
    }

    for step in spec["steps"].as_array().expect("steps") {
        match step["op"].as_str().expect("op") {
            "text" => {
                let component = text_component(step);
                if mode == "alt" {
                    alt_screen.set_layout_root(Some(component));
                } else {
                    main_screen.base.add_child(component);
                }
            }
            "render" => {
                if mode == "alt" {
                    alt_screen.reset_render_state();
                    alt_screen.do_render(&mut terminal);
                } else {
                    main_screen.base.reset_forced_render_state();
                    main_screen.base.do_render(&mut terminal);
                }
            }
            "resize" => {
                terminal.columns = step["cols"].as_u64().expect("cols") as u16;
                terminal.rows = step["rows"].as_u64().expect("rows") as u16;
            }
            "wheel" => {
                let direction = step["direction"].as_i64().expect("direction");
                let button = if direction < 0 { 64 } else { 65 };
                alt_screen.handle_mouse_input(&format!("\x1b[<{button};1;1M"), 0, &mut terminal);
            }
            "stop" => {
                let preserve = step["preserveScreen"].as_bool().unwrap_or(false);
                alt_screen.before_terminal_stop(&mut terminal, true);
                alt_screen.after_terminal_stop(&mut terminal, preserve);
            }
            other => panic!("{name}: unknown step {other}"),
        }
    }

    (terminal.out, terminal.columns, terminal.rows)
}

fn screen_json(stream: &str, columns: u16, rows: u16) -> Value {
    let mut terminal = VirtualTerminal::new(columns, rows);
    terminal.start();
    terminal.write(stream);
    serde_json::to_value(terminal.snapshot()).expect("serialize screen")
}

/// xterm's `translateToString(true)` keeps the trailing spaces of a written row while the vt100
/// port trims them; the per-cell `cells` array is compared unmodified, so only this derived
/// convenience view is normalized.
fn trim_viewport(mut screen: Value) -> Value {
    if let Some(rows) = screen.get_mut("viewport").and_then(Value::as_array_mut) {
        for row in rows {
            if let Some(text) = row.as_str() {
                *row = Value::String(text.trim_end().to_string());
            }
        }
    }
    screen
}

fn assert_case(name: &str) {
    let (stream, columns, rows) = replay(name);
    let expected_stream = std::fs::read_to_string(fixture_path(&format!("{name}.ansi")))
        .unwrap_or_else(|error| panic!("{name}.ansi: {error}"));
    if stream != expected_stream {
        let at = stream
            .char_indices()
            .zip(expected_stream.char_indices())
            .find(|((_, a), (_, b))| a != b)
            .map(|((index, _), _)| index)
            .unwrap_or_else(|| stream.len().min(expected_stream.len()));
        panic!(
            "{name}: renderer byte stream differs from the senpi fixture at byte {at}\nactual:   {:?}\nexpected: {:?}",
            stream.get(at.saturating_sub(40)..(at + 80).min(stream.len())),
            expected_stream.get(at.saturating_sub(40)..(at + 80).min(expected_stream.len()))
        );
    }

    let expected_screen: Value = serde_json::from_str(
        &std::fs::read_to_string(fixture_path(&format!("{name}.{columns}x{rows}.json")))
            .unwrap_or_else(|error| panic!("{name} screen fixture: {error}")),
    )
    .expect("screen fixture json");
    let actual_screen = screen_json(&stream, columns, rows);
    if trim_viewport(actual_screen.clone()) != trim_viewport(expected_screen.clone()) {
        panic!(
            "{name}: rendered screen differs from the senpi fixture\nactual:   {actual_screen}\nexpected: {expected_screen}"
        );
    }
}

/// A shrink must leave no stale cells: the frame rendered after a 100 -> 60 column resize has to
/// be identical to a frame rendered from scratch at 60 columns.
#[test]
fn main_screen_resize_shrink_leaves_no_stale_cells() {
    let long_line = "abcdefghij klmnopqrst uvwxyz0123 ABCDEFGHIJ KLMNOPQRST UVWXYZ4567 89";
    let build = |columns: u16| {
        let mut terminal = RecordingTerminal {
            columns,
            rows: 20,
            out: String::new(),
        };
        let mut screen = TuiMainScreen::new();
        let child: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(Text::with_padding(long_line, 0, 0)));
        screen.base.add_child(child);
        screen.base.reset_forced_render_state();
        screen.base.do_render(&mut terminal);
        terminal.out
    };

    let mut resizing = RecordingTerminal {
        columns: 100,
        rows: 20,
        out: String::new(),
    };
    let mut screen = TuiMainScreen::new();
    let child: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(Text::with_padding(long_line, 0, 0)));
    screen.base.add_child(child);
    screen.base.reset_forced_render_state();
    screen.base.do_render(&mut resizing);
    resizing.columns = 60;
    screen.base.do_render(&mut resizing);

    let after_resize = screen_json(&resizing.out, 60, 20);
    let from_scratch = screen_json(&build(60), 60, 20);
    assert_eq!(
        after_resize, from_scratch,
        "a 100 -> 60 column resize left stale cells behind"
    );

    let wide = screen_json(&resizing.out, 100, 20);
    let stale: Vec<String> = wide["viewport"]
        .as_array()
        .expect("viewport")
        .iter()
        .enumerate()
        .filter_map(|(row, line)| {
            let text = line.as_str().unwrap_or("");
            (text.chars().count() > 60).then(|| format!("row {row}: {text:?}"))
        })
        .collect();
    assert!(stale.is_empty(), "cells past column 60 survived the shrink: {stale:?}");
    println!("resize 100->60: post-resize frame equals a fresh 60-column frame; no stale cells");
}

#[test]
fn main_screen_basic_frame_matches_senpi() {
    assert_case("tui-screen-main-basic");
}

#[test]
fn main_screen_multiline_frames_and_resize_match_senpi() {
    assert_case("tui-screen-main-multiline");
}

#[test]
fn alt_screen_frames_wheel_resize_and_exit_match_senpi() {
    assert_case("tui-screen-alt-basic");
}

#[test]
fn alt_screen_preserve_screen_exit_matches_senpi() {
    assert_case("tui-screen-alt-preserve");
}
