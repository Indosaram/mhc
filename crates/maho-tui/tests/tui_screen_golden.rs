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

    // senpi's test bootstrap scrubs multiplexer markers so fixtures follow the non-mux path.
    let mut main_screen = TuiMainScreen::new();
    main_screen.base.set_mux_detector(Box::new(|| false));
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
    let expected_stream = {
        let raw = std::fs::read_to_string(fixture_path(&format!("{name}.ansi")))
            .unwrap_or_else(|error| panic!("{name}.ansi: {error}"));
        if cfg!(windows) {
            raw.replace("\x1b[3J", "")
        } else {
            raw
        }
    };
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
        screen.base.set_mux_detector(Box::new(|| false));
        let child: Rc<RefCell<dyn Component>> =
            Rc::new(RefCell::new(Text::with_padding(long_line, 0, 0)));
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
    screen.base.set_mux_detector(Box::new(|| false));
    let child: Rc<RefCell<dyn Component>> =
        Rc::new(RefCell::new(Text::with_padding(long_line, 0, 0)));
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
    assert!(
        stale.is_empty(),
        "cells past column 60 survived the shrink: {stale:?}"
    );
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

// ---- tab-width.test.ts: "keeps tab-containing overlays on one physical terminal row" ----

struct FullViewportContent;

impl Component for FullViewportContent {
    fn render(&mut self, width: usize) -> Vec<String> {
        ["base 0", "base 1", "base 2"]
            .iter()
            .map(|line| format!("{line:<width$}"))
            .collect()
    }
}

struct TabStatusOverlay;

impl Component for TabStatusOverlay {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec!["\tX".to_string()]
    }
}

#[test]
fn keeps_tab_containing_overlays_on_one_physical_terminal_row() {
    use maho_tui::tui::{OverlayOptions, SizeValue};

    let mut terminal = RecordingTerminal {
        columns: 16,
        rows: 3,
        out: String::new(),
    };
    let mut screen = TuiMainScreen::new();
    screen.base.set_mux_detector(Box::new(|| false));
    screen
        .base
        .add_child(Rc::new(RefCell::new(FullViewportContent)));
    let _overlay = screen.base.show_overlay(
        Rc::new(RefCell::new(TabStatusOverlay)),
        Some(OverlayOptions {
            width: Some(SizeValue::Absolute(4)),
            row: Some(SizeValue::Absolute(1)),
            col: Some(SizeValue::Absolute(4)),
            ..OverlayOptions::default()
        }),
    );
    screen.base.reset_forced_render_state();
    screen.base.do_render(&mut terminal);

    let mut vt = VirtualTerminal::new(16, 3);
    vt.start();
    vt.write(&terminal.out);
    let viewport: Vec<String> = vt
        .viewport()
        .into_iter()
        .map(|row| format!("{row:<16}"))
        .collect();
    assert_eq!(
        viewport,
        ["base 0          ", "base   X        ", "base 2          "]
    );
    assert!(!terminal.out.contains('\t'));
}

/// Port of senpi `sgr-row-clear.test.ts`, `tui-render-sgr-reset.test.ts` and
/// `tui-overlay-style-leak.test.ts`: row clears are followed by a style reset, and styles do not
/// leak past a line whose trailing reset sits beyond the visible width.
mod sgr_row_clear {
    use super::*;

    const ROW_CLEAR: &str = "\x1b[2K";
    const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";

    struct SharedLines(Rc<RefCell<Vec<String>>>);

    impl Component for SharedLines {
        fn render(&mut self, _width: usize) -> Vec<String> {
            self.0.borrow().clone()
        }
    }

    fn lines(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|row| row.to_string()).collect()
    }

    fn screen(
        columns: u16,
        rows: u16,
        initial: Vec<String>,
    ) -> (TuiMainScreen, RecordingTerminal, Rc<RefCell<Vec<String>>>) {
        let content = Rc::new(RefCell::new(initial));
        let mut screen = TuiMainScreen::new();
        screen.base.set_mux_detector(Box::new(|| false));
        screen
            .base
            .add_child(Rc::new(RefCell::new(SharedLines(Rc::clone(&content)))));
        let terminal = RecordingTerminal {
            columns,
            rows,
            out: String::new(),
        };
        (screen, terminal, content)
    }

    fn render(screen: &mut TuiMainScreen, terminal: &mut RecordingTerminal) {
        screen.base.reset_forced_render_state();
        screen.base.do_render(terminal);
    }

    fn assert_every_row_clear_resets(frame: &str, scenario: &str) {
        let clears: Vec<usize> = frame.match_indices(ROW_CLEAR).map(|(at, _)| at).collect();
        assert!(
            !clears.is_empty(),
            "{scenario} should exercise at least one row clear"
        );
        for at in clears {
            let after = &frame[at + ROW_CLEAR.len()..];
            assert!(
                after.starts_with(SEGMENT_RESET),
                "{scenario} emitted a row clear without an immediate SGR reset at byte {at}"
            );
        }
    }

    fn expandable_transcript(expanded: bool) -> Vec<String> {
        let mut rows = lines(&["session title", "tools"]);
        if expanded {
            rows.extend((0..16).map(|index| format!("expanded tool detail {index}")));
        }
        rows.extend((0..6).map(|index| format!("tail row {index}")));
        rows
    }

    #[test]
    fn resets_after_row_clears_on_full_render() {
        let (mut screen, mut terminal, _content) = screen(40, 5, lines(&["full render"]));
        render(&mut screen, &mut terminal);
        assert_every_row_clear_resets(&terminal.out, "full render");
    }

    #[test]
    fn resets_after_row_clears_on_scrollback_replay() {
        let (mut screen, mut terminal, content) = screen(72, 6, expandable_transcript(true));
        render(&mut screen, &mut terminal);
        terminal.out.clear();

        *content.borrow_mut() = expandable_transcript(false);
        screen.base.do_render(&mut terminal);
        assert!(
            terminal.out.contains("\x1b[3J"),
            "scenario should use scrollback replay"
        );
        assert_every_row_clear_resets(&terminal.out, "scrollback replay");
    }

    #[test]
    fn resets_after_row_clears_for_deleted_lines() {
        let (mut screen, mut terminal, content) = screen(40, 6, lines(&["a", "b", "c", "d"]));
        render(&mut screen, &mut terminal);
        terminal.out.clear();

        *content.borrow_mut() = lines(&["a", "b"]);
        screen.base.do_render(&mut terminal);
        assert_every_row_clear_resets(&terminal.out, "deleted lines");
    }

    #[test]
    fn resets_after_row_clears_for_viewport_insert_scroll() {
        let tail = ["tail0", "tail1", "tail2", "tail3", "tail4"];
        let mut initial = lines(&["p0", "p1", "p2"]);
        initial.extend(lines(&tail));
        let (mut screen, mut terminal, content) = screen(40, 5, initial);
        render(&mut screen, &mut terminal);
        terminal.out.clear();

        *content.borrow_mut() = lines(&[
            "p0", "p1", "p2", "tail0", "tail1", "tail2", "tail3", "inserted", "tail4",
        ]);
        screen.base.do_render(&mut terminal);
        assert!(
            terminal.out.contains("\x1b[1;4r"),
            "scenario should use insert-scroll"
        );
        assert_every_row_clear_resets(&terminal.out, "viewport insert-scroll");
    }

    #[test]
    fn resets_sgr_state_before_clearing_and_repainting_changed_rows() {
        let (mut screen, mut terminal, content) = screen(
            72,
            6,
            lines(&[
                "header",
                "\x1b[38;2;9;131;232mWorking\x1b[0m",
                "footer",
                "input",
            ]),
        );
        render(&mut screen, &mut terminal);
        terminal.out.clear();

        *content.borrow_mut() = lines(&[
            "header",
            "\x1b[38;2;9;131;232mWorking harder\x1b[0m",
            "footer",
            "input",
        ]);
        screen.base.do_render(&mut terminal);
        assert!(
            terminal
                .out
                .contains("\x1b[2K\x1b[0m\x1b]8;;\x07\x1b[38;2;9;131;232mWorking harder"),
            "changed-row repaint should reset terminal style state immediately after clearing the row"
        );
        assert!(
            !terminal.out.contains("\x1b[2K\x1b[38;2;9;131;232m"),
            "repaint should not write truecolor text directly after CSI 2K"
        );
    }

    struct StaticOverlay;

    impl Component for StaticOverlay {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec!["OVR".to_string()]
        }
    }

    fn cell_italic(stream: &str, columns: u16, rows: u16, row: usize, col: usize) -> bool {
        let mut vt = VirtualTerminal::new(columns, rows);
        vt.start();
        vt.write(stream);
        vt.snapshot().cells[row][col]
            .attrs
            .iter()
            .any(|attr| attr == "italic")
    }

    fn italic_base_line(width: usize) -> String {
        format!("\x1b[3m{}\x1b[23m", "X".repeat(width))
    }

    #[test]
    fn should_not_leak_styles_when_a_trailing_reset_sits_beyond_the_last_visible_column_no_overlay()
    {
        let (mut screen, mut terminal, _content) =
            screen(20, 6, vec![italic_base_line(20), "INPUT".to_string()]);
        render(&mut screen, &mut terminal);
        assert!(!cell_italic(&terminal.out, 20, 6, 1, 0));
    }

    #[test]
    fn should_not_leak_styles_when_overlay_slicing_drops_trailing_sgr_resets() {
        use maho_tui::tui::{OverlayOptions, SizeValue};

        let (mut screen, mut terminal, _content) =
            screen(20, 6, vec![italic_base_line(20), "INPUT".to_string()]);
        let _overlay = screen.base.show_overlay(
            Rc::new(RefCell::new(StaticOverlay)),
            Some(OverlayOptions {
                row: Some(SizeValue::Absolute(0)),
                col: Some(SizeValue::Absolute(5)),
                width: Some(SizeValue::Absolute(3)),
                ..OverlayOptions::default()
            }),
        );
        render(&mut screen, &mut terminal);
        assert!(!cell_italic(&terminal.out, 20, 6, 1, 0));
    }
}

/// Port of senpi `sgr-coalesce.test.ts`. The cell comparison uses the vt100-backed
/// VirtualTerminal snapshot (char, fg, bg, bold/dim/italic/underline/inverse).
mod sgr_coalesce {
    use super::*;
    use maho_tui::utils::coalesce_adjacent_sgr;

    const SEMANTIC_CORPUS: [&str; 30] = [
        "\x1b[31m\x1b[1mred bold\x1b[0m plain",
        "\x1b[1m\x1b[31mred bold reversed order\x1b[22m\x1b[39m",
        "\x1b[38;5;196m\x1b[48;5;16m256 color\x1b[0m",
        "\x1b[38;2;10;20;30m\x1b[48;2;240;230;220mtruecolor\x1b[0m",
        "\x1b[4m\x1b[24munderline reset before text",
        "\x1b[3mitalic \x1b[23mplain",
        "\x1b[1;31m\x1b[44mnested colors\x1b[0m",
        "\x1b[31mred\x1b[39m default \x1b[32mgreen\x1b[0m",
        "\x1b[48;5;22mbackground\x1b[49m default",
        "\x1b[2m\x1b[22mdim reset",
        "\x1b[7m\x1b[27minverse reset",
        "\x1b[9m\x1b[29mstrike reset",
        "\x1b[5m\x1b[25mblink reset",
        "\x1b[8m\x1b[28mhidden reset",
        "\x1b[0m\x1b[31mreset then red",
        "\x1b[m\x1b[32mempty reset then green",
        "\x1b[38;5;240mgray \x1b[38;5;241mgray2\x1b[0m",
        "\x1b[38;2;1;2;3mfg \x1b[48;2;4;5;6mbg\x1b[0m",
        "\x1b[1m\x1b[3m\x1b[4mmany attrs\x1b[0m",
        "plain \x1b[31mred\x1b[0m tail",
        "\x1b]8;;https://example.com\x07\x1b[34mlink\x1b[0m\x1b]8;;\x07",
        "\x1b]8;;https://example.com\x1b\\\x1b[34mlink st\x1b[0m\x1b]8;;\x1b\\",
        "\x1b]8;id=1;https://example.com\x07\x1b[1mparam link\x1b[0m\x1b]8;;\x07",
        "\x1b[31m\x1b]8;;https://example.com\x07mixed\x1b]8;;\x07\x1b[0m",
        "terminator \x1b]8;;https://example.com\x07x\x1b]8;;\x07 \x1b[32mgreen\x1b[0m",
        "\x1b[31mred\x1b[0m\x1b[2K\x1b[0m\x1b]8;;\x07clear guard",
        "\x1b[31mred\x1b[39m\x1b[0mreset-heavy",
        "\x1b[1mwide 漢字\x1b[22m tail",
        "\x1b[38;5;33memoji 😀\x1b[0m tail",
        "\x1b[31m\x1b[44mcombo \x1b[49mfg-only\x1b[39m",
    ];

    fn render_line(line: &str) -> Vec<maho_test_support::vterm::Cell> {
        let mut vt = VirtualTerminal::new(96, 3);
        vt.write(&format!("{line}\x1b[0m"));
        vt.snapshot().cells.swap_remove(0)
    }

    #[test]
    fn merges_immediately_adjacent_sgr_sequences_exactly() {
        for (input, expected) in [
            ("\x1b[31m\x1b[1mred", "\x1b[31;1mred"),
            ("a\x1b[38;5;196m\x1b[48;5;16mb", "a\x1b[38;5;196;48;5;16mb"),
            (
                "\x1b[38;2;1;2;3m\x1b[48;2;4;5;6mtrue",
                "\x1b[38;2;1;2;3;48;2;4;5;6mtrue",
            ),
            ("\x1b[m\x1b[31mempty-reset", "\x1b[0;31mempty-reset"),
            ("\x1b[31mtext\x1b[1m", "\x1b[31mtext\x1b[1m"),
            ("\x1b[2K\x1b[0m\x1b]8;;\x07", "\x1b[2K\x1b[0m\x1b]8;;\x07"),
        ] {
            assert_eq!(coalesce_adjacent_sgr(input), expected, "{input:?}");
        }
    }

    #[test]
    fn is_a_virtual_terminal_semantic_no_op_for_the_sgr_osc_corpus() {
        for line in SEMANTIC_CORPUS {
            assert_eq!(
                render_line(&coalesce_adjacent_sgr(line)),
                render_line(line),
                "{line:?}"
            );
        }
    }

    #[test]
    fn is_idempotent() {
        for line in SEMANTIC_CORPUS {
            let once = coalesce_adjacent_sgr(line);
            assert_eq!(coalesce_adjacent_sgr(&once), once, "{line:?}");
        }
    }

    /// senpi's `bench/sgr-coalesce-report.json` has `"adopted": false` at the pin, so the
    /// renderer must emit the original adjacent SGR bytes.
    #[test]
    fn emit_path_wiring_matches_report_adopted() {
        struct Probe;
        impl Component for Probe {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["\x1b[31m\x1b[1mcoalesce-probe".to_string()]
            }
        }
        let mut terminal = RecordingTerminal {
            columns: 60,
            rows: 4,
            out: String::new(),
        };
        let mut screen = TuiMainScreen::new();
        screen.base.set_mux_detector(Box::new(|| false));
        screen.base.add_child(Rc::new(RefCell::new(Probe)));
        screen.base.reset_forced_render_state();
        screen.base.do_render(&mut terminal);
        assert!(terminal.out.contains("\x1b[31m\x1b[1mcoalesce-probe"));
        assert!(!terminal.out.contains("\x1b[31;1mcoalesce-probe"));
    }
}

/// Port of senpi `cursor-write-hygiene.test.ts`. senpi's `start()`/`stop()` are the
/// `prepare_start`/`after_terminal_start` and `begin_stop`/`finish_stop_output`/`reset_for_stop`
/// halves the hosts call; `waitForRender` is an explicit `do_render`.
mod cursor_write_hygiene {
    use super::*;
    use maho_tui::components::input::{Input, InputOptions};
    use maho_tui::tui::CURSOR_MARKER;

    const FRAME_BEGIN: &str = "\x1b[?2026h";
    const FRAME_END: &str = "\x1b[?2026l";
    const HIDE_CURSOR: &str = "\x1b[?25l";
    const SHOW_CURSOR: &str = "\x1b[?25h";

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Kind {
        Write,
        HideCursor,
        ShowCursor,
    }

    struct ChunkTerminal {
        chunks: Vec<(Kind, String)>,
    }

    impl ChunkTerminal {
        fn new() -> Self {
            Self { chunks: Vec::new() }
        }

        fn output(&self) -> String {
            self.chunks.iter().map(|(_, data)| data.as_str()).collect()
        }

        fn screen(&self) -> VirtualTerminal {
            let mut vt = VirtualTerminal::new(40, 6);
            vt.start();
            vt.write(&self.output());
            vt
        }
    }

    impl Terminal for ChunkTerminal {
        fn start(&mut self, _on_input: InputHandler, _on_resize: ResizeHandler) {}
        fn stop(&mut self) -> Result<(), TerminalError> {
            Ok(())
        }
        fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
        fn write(&mut self, data: &str) {
            self.chunks.push((Kind::Write, data.to_string()));
        }
        fn columns(&self) -> u16 {
            40
        }
        fn rows(&self) -> u16 {
            6
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
            self.chunks
                .push((Kind::HideCursor, HIDE_CURSOR.to_string()));
        }
        fn show_cursor(&mut self) {
            self.chunks
                .push((Kind::ShowCursor, SHOW_CURSOR.to_string()));
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

    /// A focusable component whose line is `line(marker)`.
    struct MarkerLine {
        focused: bool,
        frame: usize,
        line: fn(usize, &str) -> String,
    }

    impl Component for MarkerLine {
        fn render(&mut self, _width: usize) -> Vec<String> {
            let marker = if self.focused { CURSOR_MARKER } else { "" };
            vec![(self.line)(self.frame, marker)]
        }
        fn handle_input(&mut self, _data: &str) {}
        fn has_input_handler(&self) -> bool {
            true
        }
        fn focusable_get(&self) -> Option<bool> {
            Some(self.focused)
        }
        fn focusable_set(&mut self, focused: bool) {
            self.focused = focused;
        }
    }

    struct Harness {
        screen: TuiMainScreen,
        terminal: ChunkTerminal,
    }

    impl Harness {
        fn new(show_hardware_cursor: bool, component: Rc<RefCell<dyn Component>>) -> Self {
            let mut screen = TuiMainScreen::new();
            screen.base.set_mux_detector(Box::new(|| false));
            screen.base.set_show_hardware_cursor(show_hardware_cursor);
            screen.base.add_child(Rc::clone(&component));
            screen.base.set_focus(Some(component));
            let mut harness = Self {
                screen,
                terminal: ChunkTerminal::new(),
            };
            harness.start();
            harness
        }

        fn start(&mut self) {
            self.screen.base.prepare_start();
            self.screen.base.after_terminal_start(&mut self.terminal);
            self.render();
        }

        fn render(&mut self) {
            self.screen.base.do_render(&mut self.terminal);
        }

        fn stop(&mut self) {
            self.screen.base.begin_stop(&mut self.terminal);
            self.screen.before_terminal_stop(&mut self.terminal);
            self.screen
                .base
                .finish_stop_output(&mut self.terminal, false);
            self.screen.base.reset_for_stop();
        }
    }

    fn cursor_component() -> Rc<RefCell<MarkerLine>> {
        Rc::new(RefCell::new(MarkerLine {
            focused: false,
            frame: 0,
            line: |frame, marker| format!("frame {frame} {marker}"),
        }))
    }

    fn start_hidden_cursor_tui() -> (Harness, Rc<RefCell<MarkerLine>>) {
        let component = cursor_component();
        let harness = Harness::new(false, component.clone());
        (harness, component)
    }

    fn render_next_frame(harness: &mut Harness, component: &Rc<RefCell<MarkerLine>>) {
        component.borrow_mut().frame += 1;
        harness.render();
    }

    fn count(text: &str, needle: &str) -> usize {
        text.matches(needle).count()
    }

    fn assert_atomic_frame_writes(chunks: &[(Kind, String)]) {
        let mut after_frame_end = false;
        for (kind, data) in chunks {
            if data.contains(FRAME_BEGIN) {
                assert_eq!(*kind, Kind::Write);
                assert!(data.starts_with(FRAME_BEGIN));
                assert_eq!(count(data, FRAME_BEGIN), count(data, FRAME_END));
                assert!(data.ends_with(FRAME_END));
                after_frame_end = false;
            }
            assert!(
                !after_frame_end,
                "chunk {data:?} was written outside a synchronized frame"
            );
            if data.contains(FRAME_END) {
                after_frame_end = true;
            }
        }
    }

    fn input_with(value: &str, key: &str) -> Rc<RefCell<dyn Component>> {
        let mut input = Input::new(InputOptions::default());
        input.set_value(value);
        input.handle_input(key);
        Rc::new(RefCell::new(input))
    }

    fn first_row(harness: &Harness) -> String {
        harness.terminal.screen().viewport()[0]
            .trim_end()
            .to_string()
    }

    #[test]
    fn omits_repeated_hidden_cursor_bytes_after_the_first_hidden_frame() {
        let (mut harness, component) = start_hidden_cursor_tui();
        render_next_frame(&mut harness, &component);
        let after_frame_one = count(&harness.terminal.output(), HIDE_CURSOR);
        render_next_frame(&mut harness, &component);
        render_next_frame(&mut harness, &component);
        assert_eq!(
            count(&harness.terminal.output(), HIDE_CURSOR),
            after_frame_one
        );
    }

    #[test]
    fn keeps_cursor_restoration_inside_each_synchronized_frame() {
        let (mut harness, component) = start_hidden_cursor_tui();
        render_next_frame(&mut harness, &component);
        render_next_frame(&mut harness, &component);
        assert_atomic_frame_writes(&harness.terminal.chunks);
    }

    #[test]
    fn emits_show_cursor_bytes_exactly_once_when_hardware_cursor_visibility_is_enabled() {
        let (mut harness, component) = start_hidden_cursor_tui();
        render_next_frame(&mut harness, &component);
        harness.screen.base.set_show_hardware_cursor(true);
        harness.render();
        render_next_frame(&mut harness, &component);
        assert_eq!(count(&harness.terminal.output(), SHOW_CURSOR), 1);
    }

    #[test]
    fn removes_a_focused_input_fake_cursor_when_the_hardware_cursor_is_visible() {
        let harness = Harness::new(true, input_with("한글", "\x05"));
        assert!(!harness.terminal.output().contains("\x1b[7m"));
        assert_eq!(harness.terminal.screen().cursor_position().x, 6);
        assert_eq!(first_row(&harness), "> 한글");
    }

    #[test]
    fn preserves_cursor_content_and_surrounding_styles_when_removing_fake_cursor_styling() {
        let component = Rc::new(RefCell::new(MarkerLine {
            focused: false,
            frame: 0,
            line: |_, marker| format!("\x1b[1;31mA{marker}\x1b[7m한\x1b[27mB\x1b[0m"),
        }));
        let harness = Harness::new(true, component);
        let rendered = harness.terminal.output();
        assert!(rendered.contains("\x1b[1;31mA한B\x1b[0m"));
        assert!(!rendered.contains("\x1b[7m") && !rendered.contains("\x1b[27m"));
        assert_eq!(first_row(&harness), "A한B");
    }

    #[test]
    fn removes_fake_cursor_styling_terminated_by_a_full_sgr_reset() {
        let component = Rc::new(RefCell::new(MarkerLine {
            focused: false,
            frame: 0,
            line: |_, marker| format!("\x1b[1;31mA{marker}\x1b[7mX\x1b[0mB"),
        }));
        let harness = Harness::new(true, component);
        assert_eq!(first_row(&harness), "AXB");
        let cells = harness.terminal.screen().snapshot().cells.swap_remove(0);
        let cursor = &cells[1];
        assert_eq!(cursor.ch, "X");
        assert!(!cursor.attrs.iter().any(|attr| attr == "inverse"));
        assert!(cursor.attrs.iter().any(|attr| attr == "bold"));
        assert_ne!(cursor.fg, "default");
        let trailing = &cells[2];
        assert_eq!(trailing.ch, "B");
        assert!(trailing.attrs.is_empty());
        assert_eq!(trailing.fg, "default");
    }

    #[test]
    fn keeps_visible_to_hidden_cursor_mode_toggles_inside_the_replacement_frame() {
        let mut harness = Harness::new(true, input_with("한글", "\x05"));
        let before_toggle = harness.terminal.chunks.len();
        harness.screen.base.set_show_hardware_cursor(false);
        harness.render();

        let toggle = &harness.terminal.chunks[before_toggle..];
        assert_eq!(
            toggle.iter().map(|(kind, _)| *kind).collect::<Vec<_>>(),
            [Kind::Write]
        );
        let frame = &toggle[0].1;
        assert!(frame.contains(FRAME_BEGIN));
        assert!(frame.contains("\x1b[7m"));
        let hide = frame.find(HIDE_CURSOR).expect("hide cursor in the frame");
        assert!(hide < frame.find(FRAME_END).expect("frame end"));
    }

    #[test]
    fn does_not_erase_content_under_a_visible_hardware_cursor_when_stopping() {
        let mut harness = Harness::new(true, input_with("abc", "\x1b[C"));
        let before_stop = first_row(&harness);
        harness.stop();
        assert_eq!(before_stop, "> abc");
        assert_eq!(first_row(&harness), "> abc");
    }

    #[test]
    fn does_not_erase_content_when_a_hide_cursor_replacement_frame_is_still_pending() {
        let mut harness = Harness::new(true, input_with("abc", "\x1b[C"));
        harness.screen.base.set_show_hardware_cursor(false);
        harness.stop();
        assert_eq!(first_row(&harness), "> abc");
    }

    #[test]
    fn reasserts_hidden_cursor_visibility_on_the_first_frame_after_restart() {
        let (mut harness, component) = start_hidden_cursor_tui();
        render_next_frame(&mut harness, &component);
        harness.stop();
        let before_restart = count(&harness.terminal.output(), HIDE_CURSOR);
        harness.start();
        assert_eq!(
            count(&harness.terminal.output(), HIDE_CURSOR),
            before_restart + 1
        );
    }
}

/// Port of senpi `render-contract.test.ts`. `HOME`, `PI_TUI_TEST_SEAMS` and
/// `PI_TUI_STRICT_RENDER` are layered per thread through `process_env::with_overrides`. The
/// render-error dedupe is per component name for the whole process, so every case uses its own
/// component types.
mod render_contract {
    use super::*;
    use maho_tui::process_env::with_overrides;
    use maho_tui::tui::{
        render_diagnostic_lines_scanned_for_test, render_error_log_writes_for_test,
    };
    use maho_tui::utils::visible_width;

    struct Harness {
        screen: TuiMainScreen,
        terminal: RecordingTerminal,
    }

    impl Harness {
        fn new(columns: u16, rows: u16) -> Self {
            let mut screen = TuiMainScreen::new();
            screen.base.set_mux_detector(Box::new(|| false));
            Self {
                screen,
                terminal: RecordingTerminal {
                    columns,
                    rows,
                    out: String::new(),
                },
            }
        }

        fn add(&mut self, component: Rc<RefCell<dyn Component>>) {
            self.screen.base.add_child(component);
        }

        fn drive_render(&mut self) {
            self.screen.base.do_render(&mut self.terminal);
        }

        fn viewport(&self) -> Vec<String> {
            let mut vt = VirtualTerminal::new(self.terminal.columns, self.terminal.rows);
            vt.start();
            vt.write(&self.terminal.out);
            vt.viewport()
        }
    }

    struct MutableLine(Rc<RefCell<String>>);

    impl Component for MutableLine {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec![self.0.borrow().clone()]
        }
    }

    macro_rules! throwing_component {
        ($name:ident) => {
            #[derive(Default)]
            struct $name {
                focused: bool,
            }

            impl Component for $name {
                fn render(&mut self, _width: usize) -> Vec<String> {
                    panic!("render exploded");
                }
                fn handle_input(&mut self, _data: &str) {}
                fn has_input_handler(&self) -> bool {
                    true
                }
                fn focusable_get(&self) -> Option<bool> {
                    Some(self.focused)
                }
                fn focusable_set(&mut self, focused: bool) {
                    self.focused = focused;
                }
            }
        };
    }

    throwing_component!(ThrowingComponent);
    throwing_component!(FocusedThrowingComponent);
    throwing_component!(CountedThrowingComponent);
    throwing_component!(DifferentThrowingComponent);
    throwing_component!(HostLogDirectoryThrowingComponent);

    fn seams<R>(home: Option<&str>, f: impl FnOnce() -> R) -> R {
        with_overrides(
            &[
                ("HOME", home),
                ("PI_TUI_TEST_SEAMS", Some("1")),
                ("PI_TUI_STRICT_RENDER", None),
            ],
            f,
        )
    }

    fn temp_home() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp home")
    }

    fn raw_over_wide(columns: u16) -> (Harness, Rc<RefCell<String>>) {
        let line = Rc::new(RefCell::new("short".to_string()));
        let mut harness = Harness::new(columns, 4);
        harness.add(Rc::new(RefCell::new(MutableLine(Rc::clone(&line)))));
        harness.drive_render();
        (harness, line)
    }

    #[test]
    fn renders_fallback_for_a_throwing_component_when_debug_logging_cannot_write() {
        seams(Some("/dev/null"), || {
            let mut harness = Harness::new(50, 4);
            harness.add(Rc::new(RefCell::new(ThrowingComponent::default())));
            harness.drive_render();
            assert!(
                harness
                    .viewport()
                    .join("\n")
                    .contains("[render error: ThrowingComponent]")
            );
        });
    }

    #[test]
    fn renders_fallback_for_a_throwing_component_and_continues_on_a_subsequent_frame() {
        let home = temp_home();
        seams(home.path().to_str(), || {
            let mut harness = Harness::new(50, 4);
            let component = Rc::new(RefCell::new(FocusedThrowingComponent::default()));
            let after = Rc::new(RefCell::new("after survives".to_string()));
            harness.add(component.clone());
            harness.add(Rc::new(RefCell::new(MutableLine(Rc::clone(&after)))));
            harness.screen.base.set_focus(Some(component.clone()));

            harness.drive_render();
            assert!(component.borrow().focused);
            let screen = harness.viewport().join("\n");
            assert!(screen.contains("[render error: FocusedThrowingComponent]"));
            assert!(screen.contains("after survives"));

            *after.borrow_mut() = "next frame".to_string();
            harness.drive_render();
            assert!(harness.viewport().join("\n").contains("next frame"));
        });
    }

    #[test]
    fn truncates_an_over_wide_raw_component_and_continues_in_release_with_test_seams_enabled() {
        let home = temp_home();
        seams(home.path().to_str(), || {
            let (mut harness, line) = raw_over_wide(12);
            *line.borrow_mut() = "x".repeat(30);
            harness.drive_render();
            assert_eq!(visible_width(&harness.viewport()[0]), 12);
            assert!(harness.terminal.out.contains(&"x".repeat(12)));

            *line.borrow_mut() = "ok".to_string();
            harness.drive_render();
            assert_eq!(harness.viewport()[0], "ok");
        });
    }

    #[test]
    fn truncates_an_over_wide_raw_component_when_crash_logging_cannot_write_in_release() {
        seams(Some("/dev/null"), || {
            let (mut harness, line) = raw_over_wide(12);
            *line.borrow_mut() = "x".repeat(30);
            harness.drive_render();
            assert_eq!(visible_width(&harness.viewport()[0]), 12);
            assert!(harness.terminal.out.contains(&"x".repeat(12)));
        });
    }

    #[test]
    fn writes_a_single_crash_dump_across_two_over_wide_release_frames() {
        let home = temp_home();
        seams(home.path().to_str(), || {
            let (mut harness, line) = raw_over_wide(12);
            let scans_before = render_diagnostic_lines_scanned_for_test().expect("test seam");

            *line.borrow_mut() = "x".repeat(30);
            harness.drive_render();
            let scans_after_first = render_diagnostic_lines_scanned_for_test().expect("test seam");
            let crash_log = home.path().join(".senpi/agent/senpi-crash.log");
            assert!(crash_log.exists());
            assert!(scans_after_first > scans_before);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&crash_log, std::fs::Permissions::from_mode(0o444))
                    .expect("chmod crash log");
            }

            *line.borrow_mut() = "y".repeat(30);
            harness.drive_render();
            assert_eq!(
                render_diagnostic_lines_scanned_for_test(),
                Some(scans_after_first)
            );
            assert!(harness.viewport()[0].contains(&"y".repeat(12)));
        });
    }

    #[test]
    fn logs_render_errors_once_per_component_class() {
        let home = temp_home();
        seams(home.path().to_str(), || {
            let initial = render_error_log_writes_for_test().expect("test seam");
            let mut harness = Harness::new(50, 4);
            harness.add(Rc::new(RefCell::new(CountedThrowingComponent::default())));
            harness.drive_render();
            harness.drive_render();
            assert_eq!(render_error_log_writes_for_test(), Some(initial + 1));

            harness.add(Rc::new(RefCell::new(DifferentThrowingComponent::default())));
            harness.drive_render();
            assert_eq!(render_error_log_writes_for_test(), Some(initial + 2));
        });
    }

    #[test]
    fn writes_the_render_error_diagnostic_to_the_host_provided_log_directory() {
        let home = temp_home();
        let log_directory = tempfile::tempdir().expect("log directory");
        seams(home.path().to_str(), || {
            let mut harness = Harness::new(50, 4);
            harness
                .screen
                .base
                .set_log_directory(log_directory.path().to_path_buf());
            harness.add(Rc::new(RefCell::new(
                HostLogDirectoryThrowingComponent::default(),
            )));
            harness.drive_render();
        });
        assert!(log_directory.path().join("senpi-debug.log").exists());
        assert!(!home.path().join(".senpi/agent/senpi-debug.log").exists());
    }
}

/// Port of senpi `overlay-short-content.test.ts`.
#[test]
fn should_render_overlay_when_content_is_shorter_than_terminal_height() {
    struct SimpleOverlay;
    impl Component for SimpleOverlay {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec![
                "OVERLAY_TOP".to_string(),
                "OVERLAY_MID".to_string(),
                "OVERLAY_BOT".to_string(),
            ]
        }
    }

    let mut terminal = RecordingTerminal {
        columns: 80,
        rows: 24,
        out: String::new(),
    };
    let mut screen = TuiMainScreen::new();
    screen.base.set_mux_detector(Box::new(|| false));
    screen
        .base
        .add_child(Rc::new(RefCell::new(Text::with_padding(
            "Line 1\nLine 2\nLine 3",
            0,
            0,
        ))));
    let _overlay = screen
        .base
        .show_overlay(Rc::new(RefCell::new(SimpleOverlay)), None);
    screen.base.reset_forced_render_state();
    screen.base.do_render(&mut terminal);

    let mut vt = VirtualTerminal::new(80, 24);
    vt.start();
    vt.write(&terminal.out);
    let viewport = vt.viewport();
    assert!(
        viewport.iter().any(|line| line.contains("OVERLAY")),
        "Overlay should be visible when content is shorter than terminal: {viewport:?}"
    );
}

// ---- tui-render.test.ts, viewport-render.test.ts, overlay-options.test.ts,
// ---- overlay-non-capturing.test.ts and the tui-alt-screen.test.ts search cases ----
//
// senpi drives these through `VirtualTerminal` and `await terminal.waitForRender()`; here the
// renderer writes into the vt100-backed `VirtualTerminal` and each `waitForRender` is one
// explicit `do_render`, so no case depends on timing.
mod tui_render_and_overlays {
    use super::*;
    use maho_tui::alt_screen_search::{AltScreenSearchIndex, find_alt_screen_search_matches};
    use maho_tui::process_env::with_overrides;
    use maho_tui::tui::{
        OverlayAnchor, OverlayHandle, OverlayMargin, OverlayOptions, OverlayUnfocusOptions,
        SizeValue, TuiBase,
    };

    /// senpi's test `VirtualTerminal`, logging writes like its `LoggingVirtualTerminal`.
    struct VtTerminal {
        vt: VirtualTerminal,
        writes: String,
    }

    impl VtTerminal {
        fn new(columns: u16, rows: u16) -> Self {
            let mut vt = VirtualTerminal::new(columns, rows);
            vt.start();
            Self {
                vt,
                writes: String::new(),
            }
        }

        fn viewport(&self) -> Vec<String> {
            self.vt.viewport()
        }
    }

    impl Terminal for VtTerminal {
        fn start(&mut self, _on_input: InputHandler, _on_resize: ResizeHandler) {}
        fn stop(&mut self) -> Result<(), TerminalError> {
            Ok(())
        }
        fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
        fn write(&mut self, data: &str) {
            self.writes.push_str(data);
            self.vt.write(data);
        }
        fn columns(&self) -> u16 {
            self.vt.columns()
        }
        fn rows(&self) -> u16 {
            self.vt.rows()
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

    /// senpi's `TestComponent`: a mutable line list.
    struct Lines(Rc<RefCell<Vec<String>>>);

    impl Component for Lines {
        fn render(&mut self, _width: usize) -> Vec<String> {
            self.0.borrow().clone()
        }
    }

    fn set_lines(lines: &Rc<RefCell<Vec<String>>>, values: &[&str]) {
        *lines.borrow_mut() = values.iter().map(|value| value.to_string()).collect();
    }

    fn numbered(prefix: &str, count: usize) -> Vec<String> {
        (0..count)
            .map(|index| format!("{prefix} {index}"))
            .collect()
    }

    /// A started main-screen TUI over a [`VtTerminal`] (senpi `new TuiMainScreen(terminal)` +
    /// `tui.start()` + `await terminal.waitForRender()`).
    struct Harness {
        screen: TuiMainScreen,
        terminal: VtTerminal,
    }

    impl Harness {
        fn new(columns: u16, rows: u16) -> Self {
            let mut screen = TuiMainScreen::new();
            screen.base.set_mux_detector(Box::new(|| false));
            Self {
                screen,
                terminal: VtTerminal::new(columns, rows),
            }
        }

        fn base(&mut self) -> &mut TuiBase {
            &mut self.screen.base
        }

        fn start(&mut self) {
            self.screen.base.prepare_start();
            self.screen.base.after_terminal_start(&mut self.terminal);
            self.render();
        }

        /// senpi `tui.requestRender(); await terminal.waitForRender()`.
        fn render(&mut self) {
            self.screen.base.do_render(&mut self.terminal);
        }

        /// senpi `renderAndFlush`: `tui.requestRender(true)` then a frame.
        fn render_and_flush(&mut self) {
            self.screen.base.request_render(true, 0);
            self.render();
        }

        fn send_input(&mut self, data: &str) {
            self.screen.base.handle_terminal_input(data, false);
        }

        fn resize(&mut self, columns: u16, rows: u16) {
            self.terminal.vt.resize(columns, rows);
            self.render();
        }

        fn stop(&mut self) {
            self.screen.base.begin_stop(&mut self.terminal);
            self.screen.before_terminal_stop(&mut self.terminal);
            self.screen
                .base
                .finish_stop_output(&mut self.terminal, false);
            self.screen.base.reset_for_stop();
        }

        fn viewport(&self) -> Vec<String> {
            self.terminal.viewport()
        }

        fn full_redraws(&self) -> u64 {
            self.screen.base.full_redraws()
        }
    }

    fn with_lines(
        columns: u16,
        rows: u16,
        initial: &[&str],
    ) -> (Harness, Rc<RefCell<Vec<String>>>) {
        let lines = Rc::new(RefCell::new(Vec::new()));
        set_lines(&lines, initial);
        let mut harness = Harness::new(columns, rows);
        harness
            .base()
            .add_child(Rc::new(RefCell::new(Lines(Rc::clone(&lines)))));
        (harness, lines)
    }

    // ---------- tui-render.test.ts: TUI resize handling ----------

    #[test]
    fn triggers_full_re_render_when_terminal_height_changes() {
        with_overrides(&[("TERMUX_VERSION", None)], || {
            let (mut tui, _lines) = with_lines(40, 10, &["Line 0", "Line 1", "Line 2"]);
            tui.start();
            let initial = tui.full_redraws();

            tui.resize(40, 15);

            assert!(
                tui.full_redraws() > initial,
                "height change should trigger a full redraw"
            );
            assert!(tui.viewport()[0].contains("Line 0"));
            tui.stop();
        });
    }

    #[test]
    fn skips_full_re_render_on_height_changes_in_termux() {
        with_overrides(&[("TERMUX_VERSION", Some("1"))], || {
            let (mut tui, lines) = with_lines(40, 10, &[]);
            *lines.borrow_mut() = numbered("Line", 20);
            tui.start();
            tui.terminal.writes.clear();
            let initial = tui.full_redraws();

            for height in [15, 8, 14, 11] {
                tui.resize(40, height);
            }

            assert_eq!(
                tui.full_redraws(),
                initial,
                "height change should not trigger a full redraw"
            );
            assert!(
                !tui.terminal.writes.contains("\x1b[2J"),
                "height change should not clear the screen"
            );
            assert!(
                !tui.terminal.writes.contains("\x1b[3J"),
                "height change should not clear scrollback"
            );
            // senpi also checks that "Line 19" stays visible. vt100's `set_size` drops the bottom
            // rows on a shrink where xterm scrolls them into scrollback, so that check measures
            // the emulator, not the renderer, and is not ported.
            tui.stop();
        });
    }

    #[test]
    fn triggers_full_re_render_when_terminal_width_changes() {
        let (mut tui, _lines) = with_lines(40, 10, &["Line 0", "Line 1", "Line 2"]);
        tui.start();
        let initial = tui.full_redraws();

        tui.resize(60, 10);

        assert!(
            tui.full_redraws() > initial,
            "width change should trigger a full redraw"
        );
        tui.stop();
    }

    // ---------- tui-render.test.ts: TUI content shrinkage ----------

    #[test]
    fn clears_empty_rows_when_content_shrinks_significantly() {
        let (mut tui, lines) = with_lines(
            40,
            10,
            &["Line 0", "Line 1", "Line 2", "Line 3", "Line 4", "Line 5"],
        );
        tui.base().set_clear_on_shrink(true);
        tui.start();
        let initial = tui.full_redraws();

        set_lines(&lines, &["Line 0", "Line 1"]);
        tui.render();

        assert!(
            tui.full_redraws() > initial,
            "content shrinkage should trigger a full redraw"
        );
        let viewport = tui.viewport();
        assert!(viewport[0].contains("Line 0"));
        assert!(viewport[1].contains("Line 1"));
        assert_eq!(viewport[2].trim(), "");
        assert_eq!(viewport[3].trim(), "");
        tui.stop();
    }

    #[test]
    fn handles_shrink_to_single_line() {
        let (mut tui, lines) = with_lines(40, 10, &["Line 0", "Line 1", "Line 2", "Line 3"]);
        tui.base().set_clear_on_shrink(true);
        tui.start();

        set_lines(&lines, &["Only line"]);
        tui.render();

        let viewport = tui.viewport();
        assert!(viewport[0].contains("Only line"));
        assert_eq!(viewport[1].trim(), "");
        tui.stop();
    }

    #[test]
    fn handles_shrink_to_empty() {
        let (mut tui, lines) = with_lines(40, 10, &["Line 0", "Line 1", "Line 2"]);
        tui.base().set_clear_on_shrink(true);
        tui.start();

        set_lines(&lines, &[]);
        tui.render();

        let viewport = tui.viewport();
        assert_eq!(viewport[0].trim(), "");
        assert_eq!(viewport[1].trim(), "");
        tui.stop();
    }

    // ---------- tui-render.test.ts: TUI differential rendering ----------

    #[test]
    fn tracks_cursor_correctly_when_content_shrinks_with_unchanged_remaining_lines() {
        let (mut tui, lines) =
            with_lines(40, 10, &["Line 0", "Line 1", "Line 2", "Line 3", "Line 4"]);
        tui.start();

        set_lines(&lines, &["Line 0", "Line 1", "Line 2"]);
        tui.render();
        set_lines(&lines, &["Line 0", "CHANGED", "Line 2"]);
        tui.render();

        let viewport = tui.viewport();
        assert!(viewport[1].contains("CHANGED"), "{viewport:?}");
        tui.stop();
    }

    #[test]
    fn renders_correctly_when_only_a_middle_line_changes_spinner_case() {
        let (mut tui, lines) = with_lines(40, 10, &["Header", "Working...", "Footer"]);
        tui.start();

        for frame in ["|", "/", "-", "\\"] {
            *lines.borrow_mut() = vec![
                "Header".to_string(),
                format!("Working {frame}"),
                "Footer".to_string(),
            ];
            tui.render();
            let viewport = tui.viewport();
            assert!(viewport[0].contains("Header"), "{viewport:?}");
            assert!(
                viewport[1].contains(&format!("Working {frame}")),
                "{viewport:?}"
            );
            assert!(viewport[2].contains("Footer"), "{viewport:?}");
        }
        tui.stop();
    }

    #[test]
    fn resets_styles_after_each_rendered_line() {
        let (mut tui, _lines) = with_lines(20, 6, &["\x1b[3mItalic", "Plain"]);
        tui.start();

        let screen = tui.terminal.vt.snapshot();
        assert!(
            !screen.cells[1][0].attrs.contains(&"italic".to_string()),
            "{:?}",
            screen.cells[1][0]
        );
        tui.stop();
    }

    fn assert_rows(viewport: &[String], expected: &[&str]) {
        for (row, needle) in expected.iter().enumerate() {
            assert!(viewport[row].contains(needle), "row {row}: {viewport:?}");
        }
    }

    #[test]
    fn renders_correctly_when_first_line_changes_but_rest_stays_same() {
        let (mut tui, lines) = with_lines(40, 10, &["Line 0", "Line 1", "Line 2", "Line 3"]);
        tui.start();
        set_lines(&lines, &["CHANGED", "Line 1", "Line 2", "Line 3"]);
        tui.render();
        assert_rows(&tui.viewport(), &["CHANGED", "Line 1", "Line 2", "Line 3"]);
        tui.stop();
    }

    #[test]
    fn renders_correctly_when_last_line_changes_but_rest_stays_same() {
        let (mut tui, lines) = with_lines(40, 10, &["Line 0", "Line 1", "Line 2", "Line 3"]);
        tui.start();
        set_lines(&lines, &["Line 0", "Line 1", "Line 2", "CHANGED"]);
        tui.render();
        assert_rows(&tui.viewport(), &["Line 0", "Line 1", "Line 2", "CHANGED"]);
        tui.stop();
    }

    #[test]
    fn renders_correctly_when_multiple_non_adjacent_lines_change() {
        let (mut tui, lines) =
            with_lines(40, 10, &["Line 0", "Line 1", "Line 2", "Line 3", "Line 4"]);
        tui.start();
        set_lines(
            &lines,
            &["Line 0", "CHANGED 1", "Line 2", "CHANGED 3", "Line 4"],
        );
        tui.render();
        assert_rows(
            &tui.viewport(),
            &["Line 0", "CHANGED 1", "Line 2", "CHANGED 3", "Line 4"],
        );
        tui.stop();
    }

    #[test]
    fn handles_transition_from_content_to_empty_and_back_to_content() {
        let (mut tui, lines) = with_lines(40, 10, &["Line 0", "Line 1", "Line 2"]);
        tui.start();
        assert!(tui.viewport()[0].contains("Line 0"));

        set_lines(&lines, &[]);
        tui.render();
        set_lines(&lines, &["New Line 0", "New Line 1"]);
        tui.render();

        assert_rows(&tui.viewport(), &["New Line 0", "New Line 1"]);
        tui.stop();
    }

    #[test]
    fn full_re_renders_when_deleted_lines_move_the_viewport_upward() {
        let (mut tui, lines) = with_lines(20, 5, &[]);
        *lines.borrow_mut() = numbered("Line", 12);
        tui.start();
        let initial = tui.full_redraws();

        *lines.borrow_mut() = numbered("Line", 7);
        tui.render();

        assert!(
            tui.full_redraws() > initial,
            "shrink should trigger a full redraw"
        );
        assert_eq!(
            tui.viewport(),
            vec!["Line 2", "Line 3", "Line 4", "Line 5", "Line 6"]
        );
        tui.stop();
    }

    #[test]
    fn appends_after_a_shrink_without_another_full_redraw_once_the_viewport_is_reset() {
        let (mut tui, lines) = with_lines(20, 5, &[]);
        *lines.borrow_mut() = numbered("Line", 8);
        tui.start();
        let initial = tui.full_redraws();

        set_lines(&lines, &["Line 0", "Line 1"]);
        tui.render();
        assert!(
            tui.full_redraws() > initial,
            "shrink should reset the viewport with a full redraw"
        );
        let after_shrink = tui.full_redraws();

        set_lines(&lines, &["Line 0", "Line 1", "Line 2"]);
        tui.render();

        assert_eq!(
            tui.full_redraws(),
            after_shrink,
            "append should stay on the differential path"
        );
        assert_eq!(tui.viewport(), vec!["Line 0", "Line 1", "Line 2", "", ""]);
        tui.stop();
    }

    #[test]
    fn keeps_the_viewport_anchored_when_content_expands_above_the_visible_rows() {
        let (mut tui, lines) = with_lines(40, 5, &[]);
        let stable_tail = numbered("Tail", 8);
        let mut collapsed = vec!["collapsed".to_string()];
        collapsed.extend(stable_tail.iter().cloned());
        *lines.borrow_mut() = collapsed;
        tui.start();
        assert_eq!(
            tui.viewport(),
            vec!["Tail 3", "Tail 4", "Tail 5", "Tail 6", "Tail 7"]
        );

        let mut expanded = vec!["collapsed".to_string()];
        expanded.extend(numbered("Expanded", 60));
        expanded.extend(stable_tail);
        *lines.borrow_mut() = expanded;
        tui.render();

        assert_eq!(
            tui.viewport(),
            vec!["Tail 3", "Tail 4", "Tail 5", "Tail 6", "Tail 7"]
        );
        tui.stop();
    }

    #[test]
    fn clears_stale_content_when_max_lines_rendered_was_inflated_by_a_transient_component() {
        let chat = Rc::new(RefCell::new(numbered("Chat", 15)));
        let editor = Rc::new(RefCell::new(Vec::new()));
        let editor_lines = ["Editor 0", "Editor 1", "Editor 2"];
        set_lines(&editor, &editor_lines);
        let mut tui = Harness::new(40, 10);
        tui.base()
            .add_child(Rc::new(RefCell::new(Lines(Rc::clone(&chat)))));
        tui.base()
            .add_child(Rc::new(RefCell::new(Lines(Rc::clone(&editor)))));
        tui.start();

        *editor.borrow_mut() = numbered("Selector", 8);
        tui.render();
        set_lines(&editor, &editor_lines);
        tui.render();

        let before_switch = tui.full_redraws();
        *chat.borrow_mut() = numbered("Chat", 12);
        tui.render();

        assert_eq!(
            tui.full_redraws(),
            before_switch,
            "branch switch should stay on the differential path"
        );
        let viewport = tui.viewport();
        for line in &viewport {
            for stale in ["Chat 12", "Chat 13", "Chat 14"] {
                assert!(!line.contains(stale), "stale {stale:?} in {viewport:?}");
            }
        }
        assert_eq!(
            viewport,
            vec![
                "Chat 5", "Chat 6", "Chat 7", "Chat 8", "Chat 9", "Chat 10", "Chat 11", "Editor 0",
                "Editor 1", "Editor 2",
            ]
        );
        tui.stop();
    }

    // ---------- tui-render.test.ts: TUI bounded render output ----------

    /// senpi's `BoundedWriteTerminal`: records each write separately. senpi's limit is
    /// `MAX_RENDER_WRITE_CHARS` UTF-16 units; the Rust renderer bounds writes at the same
    /// constant in bytes (`write_bounded`), which is what each write is checked against.
    struct BoundedWriteTerminal {
        writes: Vec<String>,
    }

    impl Terminal for BoundedWriteTerminal {
        fn start(&mut self, _on_input: InputHandler, _on_resize: ResizeHandler) {}
        fn stop(&mut self) -> Result<(), TerminalError> {
            Ok(())
        }
        fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
        fn write(&mut self, data: &str) {
            self.writes.push(data.to_string());
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
        fn hide_cursor(&mut self) {}
        fn show_cursor(&mut self) {}
        fn clear_line(&mut self) {}
        fn clear_from_cursor(&mut self) {}
        fn clear_screen(&mut self) {}
        fn set_title(&mut self, _title: &str) {}
        fn set_progress(&mut self, _active: bool) {}
    }

    const MAX_RENDER_WRITE_CHARS: usize = 1024 * 1024;

    fn large_kitty_line() -> String {
        format!("\x1b_Ga=T,f=100;{}\x1b\\", "A".repeat(1_200_000))
    }

    #[test]
    fn splits_a_large_full_render_without_changing_its_output() {
        let kitty_line = large_kitty_line();
        let lines = Rc::new(RefCell::new(vec![kitty_line.clone(), kitty_line.clone()]));
        let mut base = TuiBase::new();
        base.set_mux_detector(Box::new(|| false));
        base.add_child(Rc::new(RefCell::new(Lines(Rc::clone(&lines)))));
        let mut terminal = BoundedWriteTerminal { writes: Vec::new() };

        base.do_render(&mut terminal);

        assert!(
            terminal.writes.len() > 2,
            "large output should be split across writes"
        );
        assert!(
            terminal
                .writes
                .iter()
                .all(|write| write.len() <= MAX_RENDER_WRITE_CHARS)
        );
        let output = terminal.writes.concat();
        assert!(output.starts_with("\x1b[?2026h"));
        assert!(output.ends_with("\x1b[?2026l"));
        assert_eq!(output.matches(kitty_line.as_str()).count(), 2);
    }

    #[test]
    fn splits_large_differential_updates_without_a_full_redraw() {
        let lines = Rc::new(RefCell::new(vec!["before".to_string()]));
        let mut base = TuiBase::new();
        base.set_mux_detector(Box::new(|| false));
        base.add_child(Rc::new(RefCell::new(Lines(Rc::clone(&lines)))));
        let mut terminal = BoundedWriteTerminal { writes: Vec::new() };
        base.do_render(&mut terminal);
        terminal.writes.clear();

        let kitty_line = large_kitty_line();
        *lines.borrow_mut() = vec!["before".to_string(), kitty_line.clone(), kitty_line];
        base.do_render(&mut terminal);

        assert!(
            terminal.writes.len() > 2,
            "large output should be split across writes"
        );
        assert!(
            terminal
                .writes
                .iter()
                .all(|write| write.len() <= MAX_RENDER_WRITE_CHARS)
        );
        let output = terminal.writes.concat();
        assert!(output.starts_with("\x1b[?2026h"));
        assert!(output.ends_with("\x1b[?2026l"));
        assert!(
            !output.contains("\x1b[2J"),
            "the update should stay on the differential path"
        );
    }

    // ---------- tui-render.test.ts: flicker budget under streaming ----------

    struct FlickerBudgetMetrics {
        ansi_byte_ratio: f64,
        begin_2026_count: usize,
        end_2026_count: usize,
        clear_count: usize,
        full_render_true_after_init: usize,
    }

    /// senpi's `countEscapeBytes` regex: CSI, OSC and APC sequences (BEL or ST terminated) and
    /// two-byte `ESC @`..`ESC _` escapes.
    fn count_escape_bytes(text: &str) -> usize {
        let pattern = regex::Regex::new(
            r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\)|_[^\x07]*(?:\x07|\x1b\\)|[@-_])",
        )
        .expect("escape regex");
        pattern
            .find_iter(text)
            .map(|found| found.as_str().len())
            .sum()
    }

    fn run_streaming_flicker_budget() -> FlickerBudgetMetrics {
        const FULL_CLEAR: &str = "\x1b[2J\x1b[H\x1b[3J";
        let lines = Rc::new(RefCell::new(Vec::new()));
        let stable_tail = numbered("stable viewport row", 18);
        let frame = |tokens: usize| {
            let mut rows = vec![format!("streamed tokens {tokens}")];
            rows.extend(stable_tail.iter().cloned());
            rows
        };
        *lines.borrow_mut() = frame(0);
        let mut tui = Harness::new(72, 8);
        tui.base()
            .add_child(Rc::new(RefCell::new(Lines(Rc::clone(&lines)))));
        tui.start();
        tui.render_and_flush();
        let initial_clear_count = tui.terminal.writes.matches(FULL_CLEAR).count();
        let mut input_bytes = 0;

        for token_index in 0..120 {
            *lines.borrow_mut() = frame(token_index + 1);
            input_bytes += token_index.to_string().len();
            tui.render();
        }

        let writes = tui.terminal.writes.clone();
        let clear_count = writes.matches(FULL_CLEAR).count();
        let escape_bytes = count_escape_bytes(&writes);
        let content_bytes = input_bytes.max(writes.len().saturating_sub(escape_bytes));
        tui.stop();
        FlickerBudgetMetrics {
            ansi_byte_ratio: escape_bytes as f64 / content_bytes.max(1) as f64,
            begin_2026_count: writes.matches("\x1b[?2026h").count(),
            end_2026_count: writes.matches("\x1b[?2026l").count(),
            clear_count,
            full_render_true_after_init: clear_count.saturating_sub(initial_clear_count),
        }
    }

    #[test]
    fn only_one_full_render_clear_after_init() {
        assert_eq!(run_streaming_flicker_budget().clear_count, 1);
    }

    #[test]
    fn ansi_byte_ratio_under_streaming_load() {
        let ratio = run_streaming_flicker_budget().ansi_byte_ratio;
        assert!(
            ratio < 1.5,
            "ANSI byte ratio should stay below 1.5, got {ratio}"
        );
    }

    #[test]
    fn decset_2026_begin_end_balance() {
        let metrics = run_streaming_flicker_budget();
        assert_eq!(metrics.begin_2026_count, metrics.end_2026_count);
    }

    #[test]
    fn zero_full_render_true_invocations_after_init() {
        assert_eq!(
            run_streaming_flicker_budget().full_render_true_after_init,
            0
        );
    }

    // ---------- viewport-render.test.ts: byte identity with the flag on and off ----------

    fn capture_bytes(enabled: bool, run: impl Fn(&mut TuiBase, &mut VtTerminal)) -> String {
        let flag = if enabled { "1" } else { "0" };
        with_overrides(&[("PI_TUI_VIEWPORT_RENDER", Some(flag))], || {
            let mut base = TuiBase::new();
            base.set_mux_detector(Box::new(|| false));
            let mut terminal = VtTerminal::new(72, 8);
            run(&mut base, &mut terminal);
            terminal.writes
        })
    }

    #[test]
    fn keeps_streaming_bytes_identical_when_enabled() {
        let run = |base: &mut TuiBase, terminal: &mut VtTerminal| {
            let lines = Rc::new(RefCell::new(Vec::new()));
            base.add_child(Rc::new(RefCell::new(Lines(Rc::clone(&lines)))));
            let stable_tail = numbered("stable viewport row", 18);
            for token in 0..=12 {
                let mut frame = vec![format!("streamed tokens {token}")];
                frame.extend(stable_tail.iter().cloned());
                *lines.borrow_mut() = frame;
                base.do_render(terminal);
            }
        };
        assert_eq!(capture_bytes(true, run), capture_bytes(false, run));
    }

    #[test]
    fn escapes_to_byte_identical_behavior_for_offscreen_line_count_changes() {
        let run = |base: &mut TuiBase, terminal: &mut VtTerminal| {
            let lines = Rc::new(RefCell::new(Vec::new()));
            base.add_child(Rc::new(RefCell::new(Lines(Rc::clone(&lines)))));
            let tail = numbered("tail row", 6);
            let mut expanded = vec!["session title".to_string(), "tools".to_string()];
            expanded.extend(numbered("expanded tool detail", 16));
            expanded.extend(tail.iter().cloned());
            *lines.borrow_mut() = expanded;
            base.do_render(terminal);
            let mut collapsed = vec!["session title".to_string(), "tools".to_string()];
            collapsed.extend(tail);
            *lines.borrow_mut() = collapsed;
            base.do_render(terminal);
        };
        assert_eq!(capture_bytes(true, run), capture_bytes(false, run));
    }

    #[test]
    fn escapes_to_byte_identical_behavior_for_offscreen_in_place_mutations() {
        let run = |base: &mut TuiBase, terminal: &mut VtTerminal| {
            let lines = Rc::new(RefCell::new(numbered("line", 60)));
            base.add_child(Rc::new(RefCell::new(Lines(Rc::clone(&lines)))));
            base.do_render(terminal);
            lines.borrow_mut()[0] = "line zero changed offscreen".to_string();
            base.do_render(terminal);
        };
        assert_eq!(capture_bytes(true, run), capture_bytes(false, run));
    }

    // ---------- overlay-options.test.ts ----------

    /// senpi's `StaticOverlay`: fixed lines, records the width it was rendered at.
    struct StaticOverlay {
        lines: Vec<String>,
        requested_width: Rc<RefCell<Option<usize>>>,
    }

    impl Component for StaticOverlay {
        fn render(&mut self, width: usize) -> Vec<String> {
            *self.requested_width.borrow_mut() = Some(width);
            self.lines.clone()
        }
    }

    fn static_overlay(lines: &[&str]) -> Rc<RefCell<dyn Component>> {
        static_overlay_tracked(lines).0
    }

    fn static_overlay_tracked(
        lines: &[&str],
    ) -> (Rc<RefCell<dyn Component>>, Rc<RefCell<Option<usize>>>) {
        let requested_width = Rc::new(RefCell::new(None));
        let overlay: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(StaticOverlay {
            lines: lines.iter().map(|line| line.to_string()).collect(),
            requested_width: Rc::clone(&requested_width),
        }));
        (overlay, requested_width)
    }

    /// senpi's `EmptyContent`.
    struct Empty;

    impl Component for Empty {
        fn render(&mut self, _width: usize) -> Vec<String> {
            Vec::new()
        }
    }

    fn empty_harness(columns: u16, rows: u16) -> Harness {
        let mut harness = Harness::new(columns, rows);
        harness.base().add_child(Rc::new(RefCell::new(Empty)));
        harness
    }

    fn options() -> OverlayOptions {
        OverlayOptions::default()
    }

    fn show_and_render(
        columns: u16,
        rows: u16,
        overlay: Rc<RefCell<dyn Component>>,
        opts: OverlayOptions,
    ) -> Harness {
        let mut tui = empty_harness(columns, rows);
        tui.base().show_overlay(overlay, Some(opts));
        tui.start();
        tui.render_and_flush();
        tui
    }

    fn col_of(line: &str, needle: &str) -> Option<usize> {
        line.find(needle).map(|byte| line[..byte].chars().count())
    }

    #[test]
    fn should_truncate_overlay_lines_that_exceed_declared_width() {
        let wide = "X".repeat(100);
        let mut tui = show_and_render(
            80,
            24,
            static_overlay(&[wide.as_str()]),
            OverlayOptions {
                width: Some(SizeValue::Absolute(20)),
                ..options()
            },
        );
        assert_eq!(tui.viewport().len(), 24);
        tui.stop();
    }

    #[test]
    fn should_handle_overlay_with_complex_ansi_sequences_without_crashing() {
        let complex = format!(
            "\x1b[48;2;40;50;40m \x1b[38;2;128;128;128mSome styled content\x1b[39m\x1b[49m\x1b]8;;http://example.com\x07link\x1b]8;;\x07{}",
            " more content ".repeat(10)
        );
        let mut tui = show_and_render(
            80,
            24,
            static_overlay(&[complex.as_str(), complex.as_str(), complex.as_str()]),
            OverlayOptions {
                width: Some(SizeValue::Absolute(60)),
                ..options()
            },
        );
        assert!(!tui.viewport().is_empty());
        tui.stop();
    }

    #[test]
    fn should_handle_overlay_composited_on_styled_base_content() {
        struct StyledContent;
        impl Component for StyledContent {
            fn render(&mut self, width: usize) -> Vec<String> {
                let line = format!("\x1b[1m\x1b[38;2;255;0;0m{}\x1b[0m", "X".repeat(width));
                vec![line.clone(), line.clone(), line]
            }
        }
        let mut tui = Harness::new(80, 24);
        tui.base().add_child(Rc::new(RefCell::new(StyledContent)));
        tui.base().show_overlay(
            static_overlay(&["OVERLAY"]),
            Some(OverlayOptions {
                width: Some(SizeValue::Absolute(20)),
                anchor: Some(OverlayAnchor::Center),
                ..options()
            }),
        );
        tui.start();
        tui.render_and_flush();
        assert!(tui.viewport().iter().any(|line| line.contains("OVERLAY")));
        tui.stop();
    }

    #[test]
    fn should_handle_wide_characters_at_overlay_boundary() {
        let mut tui = show_and_render(
            80,
            24,
            static_overlay(&["中文日本語汉字テスト漢字"]),
            OverlayOptions {
                width: Some(SizeValue::Absolute(15)),
                ..options()
            },
        );
        assert!(!tui.viewport().is_empty());
        tui.stop();
    }

    #[test]
    fn should_handle_overlay_positioned_at_terminal_edge() {
        let wide = "X".repeat(50);
        let mut tui = show_and_render(
            80,
            24,
            static_overlay(&[wide.as_str()]),
            OverlayOptions {
                col: Some(SizeValue::Absolute(60)),
                width: Some(SizeValue::Absolute(20)),
                ..options()
            },
        );
        assert!(!tui.viewport().is_empty());
        tui.stop();
    }

    #[test]
    fn should_handle_overlay_on_base_content_with_osc_sequences() {
        struct HyperlinkContent;
        impl Component for HyperlinkContent {
            fn render(&mut self, width: usize) -> Vec<String> {
                let link = "\x1b]8;;file:///path/to/file.ts\x07file.ts\x1b]8;;\x07";
                let line = format!("See {link} for details {}", "X".repeat(width - 30));
                vec![line.clone(), line.clone(), line]
            }
        }
        let mut tui = Harness::new(80, 24);
        tui.base()
            .add_child(Rc::new(RefCell::new(HyperlinkContent)));
        tui.base().show_overlay(
            static_overlay(&["OVERLAY-TEXT"]),
            Some(OverlayOptions {
                anchor: Some(OverlayAnchor::Center),
                width: Some(SizeValue::Absolute(20)),
                ..options()
            }),
        );
        tui.start();
        tui.render_and_flush();
        assert!(!tui.viewport().is_empty());
        tui.stop();
    }

    #[test]
    fn should_render_overlay_at_percentage_of_terminal_width() {
        let (overlay, requested_width) = static_overlay_tracked(&["test"]);
        let mut tui = show_and_render(
            100,
            24,
            overlay,
            OverlayOptions {
                width: Some(SizeValue::Percent(50.0)),
                ..options()
            },
        );
        assert_eq!(*requested_width.borrow(), Some(50));
        tui.stop();
    }

    #[test]
    fn should_respect_min_width_when_width_percent_results_in_smaller_width() {
        let (overlay, requested_width) = static_overlay_tracked(&["test"]);
        let mut tui = show_and_render(
            100,
            24,
            overlay,
            OverlayOptions {
                width: Some(SizeValue::Percent(10.0)),
                min_width: Some(30),
                ..options()
            },
        );
        assert_eq!(*requested_width.borrow(), Some(30));
        tui.stop();
    }

    fn anchored(anchor: OverlayAnchor, width: i64) -> OverlayOptions {
        OverlayOptions {
            anchor: Some(anchor),
            width: Some(SizeValue::Absolute(width)),
            ..options()
        }
    }

    #[test]
    fn should_position_overlay_at_top_left() {
        let mut tui = show_and_render(
            80,
            24,
            static_overlay(&["TOP-LEFT"]),
            anchored(OverlayAnchor::TopLeft, 10),
        );
        assert!(
            tui.viewport()[0].starts_with("TOP-LEFT"),
            "{:?}",
            tui.viewport()[0]
        );
        tui.stop();
    }

    #[test]
    fn should_position_overlay_at_bottom_right() {
        let mut tui = show_and_render(
            80,
            24,
            static_overlay(&["BTM-RIGHT"]),
            anchored(OverlayAnchor::BottomRight, 10),
        );
        let last_row = tui.viewport()[23].clone();
        assert!(last_row.trim_end().ends_with("BTM-RIGHT"), "{last_row:?}");
        tui.stop();
    }

    #[test]
    fn should_position_overlay_at_top_center() {
        let mut tui = show_and_render(
            80,
            24,
            static_overlay(&["CENTERED"]),
            anchored(OverlayAnchor::TopCenter, 10),
        );
        let col = col_of(&tui.viewport()[0], "CENTERED").expect("CENTERED on the first row");
        assert!((30..=40).contains(&col), "centered col {col}");
        tui.stop();
    }

    fn margin(top: i64, right: i64, bottom: i64, left: i64) -> Option<OverlayMargin> {
        Some(OverlayMargin {
            top: Some(top),
            right: Some(right),
            bottom: Some(bottom),
            left: Some(left),
        })
    }

    #[test]
    fn should_clamp_negative_margins_to_zero() {
        let mut tui = show_and_render(
            80,
            24,
            static_overlay(&["NEG-MARGIN"]),
            OverlayOptions {
                margin: margin(-5, 0, 0, -10),
                ..anchored(OverlayAnchor::TopLeft, 12)
            },
        );
        assert!(
            tui.viewport()[0].starts_with("NEG-MARGIN"),
            "{:?}",
            tui.viewport()[0]
        );
        tui.stop();
    }

    /// senpi passes `margin: 5`; `OverlayMargin` has no scalar form, so all four sides are 5.
    #[test]
    fn should_respect_margin_as_number() {
        let mut tui = show_and_render(
            80,
            24,
            static_overlay(&["MARGIN"]),
            OverlayOptions {
                margin: margin(5, 5, 5, 5),
                ..anchored(OverlayAnchor::TopLeft, 10)
            },
        );
        let viewport = tui.viewport();
        assert!(!viewport[0].contains("MARGIN"));
        assert!(!viewport[4].contains("MARGIN"));
        assert_eq!(col_of(&viewport[5], "MARGIN"), Some(5), "{viewport:?}");
        tui.stop();
    }

    #[test]
    fn should_respect_margin_object() {
        let mut tui = show_and_render(
            80,
            24,
            static_overlay(&["MARGIN"]),
            OverlayOptions {
                margin: margin(2, 0, 0, 3),
                ..anchored(OverlayAnchor::TopLeft, 10)
            },
        );
        assert_eq!(
            col_of(&tui.viewport()[2], "MARGIN"),
            Some(3),
            "{:?}",
            tui.viewport()
        );
        tui.stop();
    }

    #[test]
    fn should_apply_offset_x_and_offset_y_from_anchor_position() {
        let mut tui = show_and_render(
            80,
            24,
            static_overlay(&["OFFSET"]),
            OverlayOptions {
                offset_x: Some(10),
                offset_y: Some(5),
                ..anchored(OverlayAnchor::TopLeft, 10)
            },
        );
        assert_eq!(
            col_of(&tui.viewport()[5], "OFFSET"),
            Some(10),
            "{:?}",
            tui.viewport()
        );
        tui.stop();
    }

    #[test]
    fn should_position_with_row_percent_and_col_percent() {
        let mut tui = show_and_render(
            80,
            24,
            static_overlay(&["PCT"]),
            OverlayOptions {
                width: Some(SizeValue::Absolute(10)),
                row: Some(SizeValue::Percent(50.0)),
                col: Some(SizeValue::Percent(50.0)),
                ..options()
            },
        );
        let row = tui.viewport().iter().position(|line| line.contains("PCT"));
        assert!(matches!(row, Some(10..=13)), "centered row {row:?}");
        tui.stop();
    }

    #[test]
    fn row_percent_0_should_position_at_top() {
        let mut tui = show_and_render(
            80,
            24,
            static_overlay(&["TOP"]),
            OverlayOptions {
                width: Some(SizeValue::Absolute(10)),
                row: Some(SizeValue::Percent(0.0)),
                ..options()
            },
        );
        assert!(tui.viewport()[0].contains("TOP"));
        tui.stop();
    }

    #[test]
    fn row_percent_100_should_position_at_bottom() {
        let mut tui = show_and_render(
            80,
            24,
            static_overlay(&["BOTTOM"]),
            OverlayOptions {
                width: Some(SizeValue::Absolute(10)),
                row: Some(SizeValue::Percent(100.0)),
                ..options()
            },
        );
        assert!(tui.viewport()[23].contains("BOTTOM"));
        tui.stop();
    }

    #[test]
    fn should_truncate_overlay_to_max_height() {
        let mut tui = show_and_render(
            80,
            24,
            static_overlay(&["Line 1", "Line 2", "Line 3", "Line 4", "Line 5"]),
            OverlayOptions {
                max_height: Some(SizeValue::Absolute(3)),
                ..options()
            },
        );
        let content = tui.viewport().join("\n");
        for kept in ["Line 1", "Line 2", "Line 3"] {
            assert!(content.contains(kept), "{content}");
        }
        for dropped in ["Line 4", "Line 5"] {
            assert!(!content.contains(dropped), "{content}");
        }
        tui.stop();
    }

    #[test]
    fn should_truncate_overlay_to_max_height_percent() {
        let mut tui = show_and_render(
            80,
            10,
            static_overlay(&["L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8", "L9", "L10"]),
            OverlayOptions {
                max_height: Some(SizeValue::Percent(50.0)),
                ..options()
            },
        );
        let content = tui.viewport().join("\n");
        assert!(content.contains("L1"));
        assert!(content.contains("L5"));
        assert!(!content.contains("L6"));
        tui.stop();
    }

    #[test]
    fn row_and_col_should_override_anchor() {
        let mut tui = show_and_render(
            80,
            24,
            static_overlay(&["ABSOLUTE"]),
            OverlayOptions {
                row: Some(SizeValue::Absolute(3)),
                col: Some(SizeValue::Absolute(5)),
                ..anchored(OverlayAnchor::BottomRight, 10)
            },
        );
        assert_eq!(
            col_of(&tui.viewport()[3], "ABSOLUTE"),
            Some(5),
            "{:?}",
            tui.viewport()
        );
        tui.stop();
    }

    #[test]
    fn should_render_multiple_overlays_with_later_ones_on_top() {
        let mut tui = empty_harness(80, 24);
        tui.base().show_overlay(
            static_overlay(&["FIRST-OVERLAY"]),
            Some(anchored(OverlayAnchor::TopLeft, 20)),
        );
        tui.base().show_overlay(
            static_overlay(&["SECOND"]),
            Some(anchored(OverlayAnchor::TopLeft, 10)),
        );
        tui.start();
        tui.render_and_flush();
        assert!(
            tui.viewport()[0].contains("SECOND"),
            "{:?}",
            tui.viewport()[0]
        );
        tui.stop();
    }

    #[test]
    fn should_handle_overlays_at_different_positions_without_interference() {
        let mut tui = empty_harness(80, 24);
        tui.base().show_overlay(
            static_overlay(&["TOP-LEFT"]),
            Some(anchored(OverlayAnchor::TopLeft, 15)),
        );
        tui.base().show_overlay(
            static_overlay(&["BTM-RIGHT"]),
            Some(anchored(OverlayAnchor::BottomRight, 15)),
        );
        tui.start();
        tui.render_and_flush();
        let viewport = tui.viewport();
        assert!(viewport[0].contains("TOP-LEFT"), "{viewport:?}");
        assert!(viewport[23].contains("BTM-RIGHT"), "{viewport:?}");
        tui.stop();
    }

    #[test]
    fn should_properly_hide_overlays_in_stack_order() {
        let mut tui = empty_harness(80, 24);
        tui.base().show_overlay(
            static_overlay(&["FIRST"]),
            Some(anchored(OverlayAnchor::TopLeft, 10)),
        );
        tui.base().show_overlay(
            static_overlay(&["SECOND"]),
            Some(anchored(OverlayAnchor::TopLeft, 10)),
        );
        tui.start();
        tui.render_and_flush();
        assert!(tui.viewport()[0].contains("SECOND"));

        tui.base().hide_overlay();
        tui.render_and_flush();
        assert!(
            tui.viewport()[0].contains("FIRST"),
            "{:?}",
            tui.viewport()[0]
        );
        tui.stop();
    }

    fn non_capturing() -> OverlayOptions {
        OverlayOptions {
            non_capturing: true,
            ..options()
        }
    }

    #[test]
    fn hide_overlay_leaves_the_cursor_visible() {
        let mut tui = Harness::new(80, 24);
        tui.start();
        tui.base()
            .show_overlay(static_overlay(&["OVERLAY"]), Some(non_capturing()));

        tui.stop();
        tui.base().hide_overlay();
        tui.render();

        assert!(!tui.terminal.vt.cursor_hidden());
    }

    #[test]
    fn overlay_handle_hide_leaves_the_cursor_visible() {
        let mut tui = Harness::new(80, 24);
        tui.start();
        let handle = tui
            .base()
            .show_overlay(static_overlay(&["OVERLAY"]), Some(non_capturing()));

        tui.stop();
        handle.hide();
        tui.render();

        assert!(!tui.terminal.vt.cursor_hidden());
    }

    // ---------- overlay-non-capturing.test.ts ----------

    /// senpi's `FocusableOverlay`: records focus and input.
    #[derive(Default)]
    struct Probe {
        focused: bool,
        inputs: Vec<String>,
    }

    struct ProbeComponent {
        label: String,
        probe: Rc<RefCell<Probe>>,
    }

    impl Component for ProbeComponent {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec![self.label.clone()]
        }
        fn handle_input(&mut self, data: &str) {
            self.probe.borrow_mut().inputs.push(data.to_string());
        }
        fn has_input_handler(&self) -> bool {
            true
        }
        fn focusable_get(&self) -> Option<bool> {
            Some(self.probe.borrow().focused)
        }
        fn focusable_set(&mut self, focused: bool) {
            self.probe.borrow_mut().focused = focused;
        }
    }

    #[derive(Clone)]
    struct Focusable {
        component: Rc<RefCell<dyn Component>>,
        probe: Rc<RefCell<Probe>>,
    }

    impl Focusable {
        fn new(label: &str) -> Self {
            let probe = Rc::new(RefCell::new(Probe::default()));
            Self {
                component: Rc::new(RefCell::new(ProbeComponent {
                    label: label.to_string(),
                    probe: Rc::clone(&probe),
                })),
                probe,
            }
        }

        fn focused(&self) -> bool {
            self.probe.borrow().focused
        }

        fn inputs(&self) -> Vec<String> {
            self.probe.borrow().inputs.clone()
        }

        fn rc(&self) -> Rc<RefCell<dyn Component>> {
            Rc::clone(&self.component)
        }
    }

    /// `new TuiMainScreen` + `EmptyContent` + optional `setFocus(editor)` + `start()`.
    fn focus_harness(editor: Option<&Focusable>) -> Harness {
        let mut tui = empty_harness(80, 24);
        if let Some(editor) = editor {
            tui.base().set_focus(Some(editor.rc()));
        }
        tui.start();
        tui
    }

    fn show(tui: &mut Harness, overlay: &Focusable, opts: Option<OverlayOptions>) -> OverlayHandle {
        tui.base().show_overlay(overlay.rc(), opts)
    }

    fn unfocus_to(target: Option<&Focusable>) -> Option<OverlayUnfocusOptions> {
        Some(OverlayUnfocusOptions {
            target: target.map(Focusable::rc),
        })
    }

    #[test]
    fn non_capturing_overlay_preserves_focus_on_creation() {
        let editor = Focusable::new("EDITOR");
        let overlay = Focusable::new("OVERLAY");
        let mut tui = focus_harness(Some(&editor));
        show(&mut tui, &overlay, Some(non_capturing()));
        tui.render_and_flush();
        assert!(editor.focused());
        assert!(!overlay.focused());
        tui.stop();
    }

    #[test]
    fn focus_transfers_focus_to_the_overlay() {
        let editor = Focusable::new("EDITOR");
        let overlay = Focusable::new("OVERLAY");
        let mut tui = focus_harness(Some(&editor));
        let handle = show(&mut tui, &overlay, Some(non_capturing()));
        handle.focus();
        tui.render_and_flush();
        assert!(!editor.focused());
        assert!(overlay.focused());
        assert!(handle.is_focused());
        tui.stop();
    }

    #[test]
    fn unfocus_restores_previous_focus() {
        let editor = Focusable::new("EDITOR");
        let overlay = Focusable::new("OVERLAY");
        let mut tui = focus_harness(Some(&editor));
        let handle = show(&mut tui, &overlay, Some(non_capturing()));
        handle.focus();
        handle.unfocus(None);
        tui.render_and_flush();
        assert!(editor.focused());
        assert!(!overlay.focused());
        assert!(!handle.is_focused());
        tui.stop();
    }

    #[test]
    fn set_hidden_false_on_non_capturing_overlay_does_not_auto_focus() {
        let editor = Focusable::new("EDITOR");
        let overlay = Focusable::new("OVERLAY");
        let mut tui = focus_harness(Some(&editor));
        let handle = show(&mut tui, &overlay, Some(non_capturing()));
        handle.set_hidden(true);
        handle.set_hidden(false);
        tui.render_and_flush();
        assert!(editor.focused());
        assert!(!overlay.focused());
        tui.stop();
    }

    #[test]
    fn hide_when_overlay_is_not_focused_does_not_change_focus() {
        let editor = Focusable::new("EDITOR");
        let overlay = Focusable::new("OVERLAY");
        let mut tui = focus_harness(Some(&editor));
        let handle = show(&mut tui, &overlay, Some(non_capturing()));
        handle.hide();
        tui.render_and_flush();
        assert!(editor.focused());
        tui.stop();
    }

    #[test]
    fn hide_when_focused_restores_focus_correctly() {
        let editor = Focusable::new("EDITOR");
        let overlay = Focusable::new("OVERLAY");
        let mut tui = focus_harness(Some(&editor));
        let handle = show(&mut tui, &overlay, Some(non_capturing()));
        handle.focus();
        handle.hide();
        tui.render_and_flush();
        assert!(editor.focused());
        assert!(!overlay.focused());
        tui.stop();
    }

    #[test]
    fn capturing_overlay_removed_with_non_capturing_below_restores_focus_to_editor() {
        let editor = Focusable::new("EDITOR");
        let nc = Focusable::new("NC");
        let capturing = Focusable::new("CAP");
        let mut tui = focus_harness(Some(&editor));
        show(&mut tui, &nc, Some(non_capturing()));
        let handle = show(&mut tui, &capturing, None);
        assert!(capturing.focused());
        handle.hide();
        tui.render_and_flush();
        assert!(editor.focused());
        assert!(!nc.focused());
        tui.stop();
    }

    #[test]
    fn sub_overlay_cleanup_then_hide_overlay_restores_focus_and_input_to_editor() {
        let editor = Focusable::new("EDITOR");
        let timer = Focusable::new("TIMER");
        let controller = Focusable::new("CTRL");
        let mut tui = focus_harness(Some(&editor));
        let timer_handle = show(&mut tui, &timer, Some(non_capturing()));
        show(&mut tui, &controller, None);
        assert!(controller.focused());
        assert!(!editor.focused());
        timer_handle.hide();
        tui.base().hide_overlay();
        tui.render_and_flush();
        assert!(editor.focused());
        assert!(!controller.focused());
        assert!(!timer.focused());
        tui.send_input("x");
        tui.render_and_flush();
        assert_eq!(editor.inputs(), vec!["x"]);
        assert!(controller.inputs().is_empty());
        assert!(timer.inputs().is_empty());
        tui.stop();
    }

    #[test]
    fn removed_focused_child_overlay_does_not_become_parent_overlay_fallback() {
        let editor = Focusable::new("EDITOR");
        let child = Focusable::new("CHILD");
        let parent = Focusable::new("PARENT");
        let mut tui = focus_harness(Some(&editor));
        let child_handle = show(&mut tui, &child, Some(non_capturing()));
        child_handle.focus();
        let parent_handle = show(&mut tui, &parent, None);
        assert!(parent.focused());

        child_handle.hide();
        parent_handle.hide();
        tui.send_input("x");
        tui.render_and_flush();

        assert_eq!(editor.inputs(), vec!["x"]);
        assert!(child.inputs().is_empty());
        assert!(parent.inputs().is_empty());
        assert!(editor.focused());
        tui.stop();
    }

    fn visible_when(flag: &Rc<std::cell::Cell<bool>>) -> OverlayOptions {
        let flag = Rc::clone(flag);
        OverlayOptions {
            visible: Some(Rc::new(move |_columns: u16, _rows: u16| flag.get())),
            ..options()
        }
    }

    #[test]
    fn handle_input_redirection_skips_non_capturing_overlays_when_focused_overlay_becomes_invisible()
     {
        let editor = Focusable::new("EDITOR");
        let fallback = Focusable::new("FALLBACK");
        let nc = Focusable::new("NC");
        let primary = Focusable::new("PRIMARY");
        let visible = Rc::new(std::cell::Cell::new(true));
        let mut tui = focus_harness(Some(&editor));
        show(&mut tui, &fallback, None);
        show(&mut tui, &nc, Some(non_capturing()));
        show(&mut tui, &primary, Some(visible_when(&visible)));
        assert!(primary.focused());
        visible.set(false);
        tui.send_input("x");
        tui.render_and_flush();
        assert!(primary.inputs().is_empty());
        assert!(nc.inputs().is_empty());
        assert_eq!(fallback.inputs(), vec!["x"]);
        assert!(fallback.focused());
        tui.stop();
    }

    #[test]
    fn handle_input_restores_focus_to_a_visible_focused_overlay_after_base_focus_steal() {
        let editor = Focusable::new("EDITOR");
        let replacement = Focusable::new("REPLACEMENT");
        let overlay = Focusable::new("OVERLAY");
        let mut tui = focus_harness(Some(&editor));
        show(&mut tui, &overlay, None);
        assert!(overlay.focused());
        tui.base().set_focus(Some(replacement.rc()));
        tui.base().set_focus(Some(editor.rc()));
        tui.send_input("x");
        tui.render_and_flush();
        assert_eq!(overlay.inputs(), vec!["x"]);
        assert!(editor.inputs().is_empty());
        assert!(overlay.focused());
        tui.stop();
    }

    #[test]
    fn handle_input_restores_focus_to_explicitly_focused_raw_sub_overlay_after_base_focus_steal() {
        let editor = Focusable::new("EDITOR");
        let controller = Focusable::new("CONTROLLER");
        let sub = Focusable::new("SUB");
        let mut tui = focus_harness(Some(&editor));
        show(&mut tui, &controller, None);
        let sub_handle = show(&mut tui, &sub, Some(non_capturing()));
        sub_handle.focus();
        tui.base().set_focus(Some(editor.rc()));
        tui.send_input("x");
        tui.render_and_flush();
        assert_eq!(sub.inputs(), vec!["x"]);
        assert!(controller.inputs().is_empty());
        assert!(editor.inputs().is_empty());
        tui.stop();
    }

    #[test]
    fn passive_non_capturing_overlay_does_not_regain_input_after_base_focus() {
        let editor = Focusable::new("EDITOR");
        let passive = Focusable::new("PASSIVE");
        let mut tui = focus_harness(Some(&editor));
        show(&mut tui, &passive, Some(non_capturing()));
        tui.send_input("x");
        tui.render_and_flush();
        assert_eq!(editor.inputs(), vec!["x"]);
        assert!(passive.inputs().is_empty());
        assert!(editor.focused());
        tui.stop();
    }

    #[test]
    fn explicitly_focused_non_capturing_overlay_regains_input_after_base_focus_steal() {
        let editor = Focusable::new("EDITOR");
        let overlay = Focusable::new("NC");
        let mut tui = focus_harness(Some(&editor));
        let handle = show(&mut tui, &overlay, Some(non_capturing()));
        handle.focus();
        tui.base().set_focus(Some(editor.rc()));
        tui.send_input("x");
        tui.render_and_flush();
        assert_eq!(overlay.inputs(), vec!["x"]);
        assert!(editor.inputs().is_empty());
        tui.stop();
    }

    #[test]
    fn unfocus_prevents_visible_overlay_from_regaining_input() {
        let editor = Focusable::new("EDITOR");
        let overlay = Focusable::new("OVERLAY");
        let mut tui = focus_harness(Some(&editor));
        let handle = show(&mut tui, &overlay, None);
        handle.unfocus(None);
        tui.send_input("x");
        tui.render_and_flush();
        assert_eq!(editor.inputs(), vec!["x"]);
        assert!(overlay.inputs().is_empty());
        assert!(editor.focused());
        tui.stop();
    }

    #[test]
    fn set_focus_null_explicitly_clears_visible_overlay_restore() {
        let overlay = Focusable::new("OVERLAY");
        let mut tui = focus_harness(None);
        show(&mut tui, &overlay, None);
        tui.base().set_focus(None);
        tui.send_input("x");
        tui.render_and_flush();
        assert!(overlay.inputs().is_empty());
        assert!(!overlay.focused());
        tui.stop();
    }

    #[test]
    fn temporarily_invisible_focused_overlay_falls_back_without_losing_restore_eligibility() {
        let editor = Focusable::new("EDITOR");
        let overlay = Focusable::new("OVERLAY");
        let visible = Rc::new(std::cell::Cell::new(true));
        let mut tui = focus_harness(Some(&editor));
        show(&mut tui, &overlay, Some(visible_when(&visible)));
        tui.base().set_focus(Some(editor.rc()));
        visible.set(false);
        tui.send_input("x");
        tui.render_and_flush();
        assert_eq!(editor.inputs(), vec!["x"]);
        assert!(overlay.inputs().is_empty());
        visible.set(true);
        tui.send_input("y");
        tui.render_and_flush();
        assert_eq!(editor.inputs(), vec!["x"]);
        assert_eq!(overlay.inputs(), vec!["y"]);
        tui.stop();
    }

    #[test]
    fn temporarily_invisible_focused_overlay_with_null_pre_focus_restores_when_visible_again() {
        let overlay = Focusable::new("OVERLAY");
        let visible = Rc::new(std::cell::Cell::new(true));
        let mut tui = focus_harness(None);
        show(&mut tui, &overlay, Some(visible_when(&visible)));
        visible.set(false);
        tui.send_input("x");
        tui.render_and_flush();
        assert!(overlay.inputs().is_empty());
        visible.set(true);
        tui.send_input("y");
        tui.render_and_flush();
        assert_eq!(overlay.inputs(), vec!["y"]);
        tui.stop();
    }

    #[test]
    fn cyclic_overlay_pre_focus_ancestry_does_not_hang_focus_changes() {
        let editor = Focusable::new("EDITOR");
        let overlay = Focusable::new("OVERLAY");
        let mut tui = focus_harness(Some(&overlay));
        let handle = show(&mut tui, &overlay, Some(non_capturing()));
        handle.focus();
        tui.base().set_focus(Some(editor.rc()));
        tui.send_input("x");
        tui.render_and_flush();
        assert_eq!(editor.inputs(), vec!["x"]);
        assert!(overlay.inputs().is_empty());
        tui.stop();
    }

    #[test]
    fn handle_input_restores_the_focus_order_top_overlay_after_base_focus_steal() {
        let editor = Focusable::new("EDITOR");
        let lower = Focusable::new("LOWER");
        let upper = Focusable::new("UPPER");
        let mut tui = focus_harness(Some(&editor));
        let lower_handle = show(&mut tui, &lower, None);
        show(&mut tui, &upper, None);
        lower_handle.focus();
        tui.base().set_focus(Some(editor.rc()));
        tui.send_input("x");
        tui.render_and_flush();
        assert_eq!(lower.inputs(), vec!["x"]);
        assert!(upper.inputs().is_empty());
        assert!(editor.inputs().is_empty());
        tui.stop();
    }

    #[test]
    fn hide_overlay_does_not_reassign_focus_when_topmost_overlay_is_non_capturing() {
        let editor = Focusable::new("EDITOR");
        let capturing = Focusable::new("CAP");
        let nc = Focusable::new("NC");
        let mut tui = focus_harness(Some(&editor));
        show(&mut tui, &capturing, None);
        show(&mut tui, &nc, Some(non_capturing()));
        assert!(capturing.focused());
        tui.base().hide_overlay();
        tui.render_and_flush();
        assert!(capturing.focused());
        tui.stop();
    }

    #[test]
    fn multiple_capturing_and_non_capturing_overlays_restore_focus_through_removals() {
        let editor = Focusable::new("EDITOR");
        let c1 = Focusable::new("C1");
        let n1 = Focusable::new("N1");
        let c2 = Focusable::new("C2");
        let n2 = Focusable::new("N2");
        let mut tui = focus_harness(Some(&editor));
        let c1_handle = show(&mut tui, &c1, None);
        show(&mut tui, &n1, Some(non_capturing()));
        let c2_handle = show(&mut tui, &c2, None);
        show(&mut tui, &n2, Some(non_capturing()));
        assert!(c2.focused());
        c2_handle.hide();
        tui.render_and_flush();
        assert!(c1.focused());
        c1_handle.hide();
        tui.render_and_flush();
        assert!(editor.focused());
        tui.stop();
    }

    #[test]
    fn capturing_overlay_unfocus_on_topmost_capturing_overlay_falls_back_to_pre_focus() {
        let editor = Focusable::new("EDITOR");
        let capturing = Focusable::new("CAP");
        let mut tui = focus_harness(Some(&editor));
        let handle = show(&mut tui, &capturing, None);
        assert!(capturing.focused());
        handle.unfocus(None);
        tui.render_and_flush();
        assert!(editor.focused());
        assert!(!capturing.focused());
        tui.stop();
    }

    #[test]
    fn focus_on_hidden_overlay_is_a_no_op() {
        let editor = Focusable::new("EDITOR");
        let overlay = Focusable::new("OVERLAY");
        let mut tui = focus_harness(Some(&editor));
        let handle = show(&mut tui, &overlay, Some(non_capturing()));
        handle.set_hidden(true);
        handle.focus();
        tui.render_and_flush();
        assert!(editor.focused());
        assert!(!handle.is_focused());
        tui.stop();
    }

    #[test]
    fn focus_after_hide_is_a_no_op() {
        let editor = Focusable::new("EDITOR");
        let overlay = Focusable::new("OVERLAY");
        let mut tui = focus_harness(Some(&editor));
        let handle = show(&mut tui, &overlay, Some(non_capturing()));
        handle.hide();
        handle.focus();
        tui.render_and_flush();
        assert!(editor.focused());
        assert!(!handle.is_focused());
        tui.stop();
    }

    #[test]
    fn unfocus_when_overlay_does_not_have_focus_is_a_no_op() {
        let editor = Focusable::new("EDITOR");
        let overlay = Focusable::new("OVERLAY");
        let mut tui = focus_harness(Some(&editor));
        let handle = show(&mut tui, &overlay, Some(non_capturing()));
        handle.unfocus(None);
        tui.render_and_flush();
        assert!(editor.focused());
        assert!(!overlay.focused());
        tui.stop();
    }

    #[test]
    fn unfocus_with_null_pre_focus_clears_focus_and_does_not_route_input_back_to_overlay() {
        let overlay = Focusable::new("OVERLAY");
        let mut tui = focus_harness(None);
        let handle = show(&mut tui, &overlay, None);
        assert!(overlay.focused());
        handle.unfocus(None);
        assert!(!overlay.focused());
        tui.send_input("x");
        tui.render_and_flush();
        assert!(overlay.inputs().is_empty());
        assert!(!handle.is_focused());
        tui.stop();
    }

    #[test]
    fn toggle_focus_between_non_capturing_overlays_then_unfocus_returns_to_editor() {
        let editor = Focusable::new("EDITOR");
        let a = Focusable::new("A");
        let b = Focusable::new("B");
        let mut tui = focus_harness(Some(&editor));
        let a_handle = show(&mut tui, &a, Some(non_capturing()));
        let b_handle = show(&mut tui, &b, Some(non_capturing()));
        a_handle.focus();
        b_handle.focus();
        a_handle.focus();
        a_handle.unfocus(None);
        tui.render_and_flush();
        assert!(editor.focused());
        assert!(!a.focused());
        assert!(!b.focused());
        tui.stop();
    }

    #[test]
    fn explicit_unfocus_target_supports_cycling_between_three_overlays_and_editor() {
        let editor = Focusable::new("EDITOR");
        let a = Focusable::new("A");
        let b = Focusable::new("B");
        let c = Focusable::new("C");
        let mut tui = focus_harness(Some(&editor));
        let a_handle = show(&mut tui, &a, None);
        let b_handle = show(&mut tui, &b, None);
        let c_handle = show(&mut tui, &c, None);
        fn type_key(tui: &mut Harness, key: &str) {
            tui.send_input(key);
            tui.render_and_flush();
        }
        a_handle.focus();
        type_key(&mut tui, "a");
        b_handle.focus();
        type_key(&mut tui, "b");
        c_handle.focus();
        type_key(&mut tui, "c");
        c_handle.unfocus(unfocus_to(Some(&editor)));
        type_key(&mut tui, "e");
        a_handle.focus();
        type_key(&mut tui, "A");
        a_handle.unfocus(unfocus_to(Some(&editor)));
        type_key(&mut tui, "E");

        assert_eq!(a.inputs(), vec!["a", "A"]);
        assert_eq!(b.inputs(), vec!["b"]);
        assert_eq!(c.inputs(), vec!["c"]);
        assert_eq!(editor.inputs(), vec!["e", "E"]);
        assert!(editor.focused());
        tui.stop();
    }

    #[test]
    fn explicit_null_unfocus_target_clears_focus_without_restoring_overlays() {
        let overlay = Focusable::new("OVERLAY");
        let mut tui = focus_harness(None);
        let handle = show(&mut tui, &overlay, None);
        handle.unfocus(unfocus_to(None));
        tui.send_input("x");
        tui.render_and_flush();
        assert!(overlay.inputs().is_empty());
        assert!(!handle.is_focused());
        tui.stop();
    }

    #[test]
    fn hiding_focused_overlay_falls_back_to_next_visual_frontmost_overlay() {
        let editor = Focusable::new("EDITOR");
        let a = Focusable::new("A");
        let b = Focusable::new("B");
        let c = Focusable::new("C");
        let mut tui = focus_harness(Some(&editor));
        let a_handle = show(&mut tui, &a, None);
        let b_handle = show(&mut tui, &b, None);
        show(&mut tui, &c, None);
        a_handle.focus();
        b_handle.focus();
        b_handle.set_hidden(true);
        tui.send_input("x");
        tui.render_and_flush();
        assert_eq!(a.inputs(), vec!["x"]);
        assert!(c.inputs().is_empty());
        assert!(a.focused());
        tui.stop();
    }

    fn corner(non_capturing: bool) -> OverlayOptions {
        OverlayOptions {
            row: Some(SizeValue::Absolute(0)),
            col: Some(SizeValue::Absolute(0)),
            width: Some(SizeValue::Absolute(1)),
            non_capturing,
            ..options()
        }
    }

    fn first_char(tui: &Harness) -> Option<char> {
        tui.viewport()[0].chars().next()
    }

    fn small_harness(editor: Option<&Focusable>) -> Harness {
        let mut tui = empty_harness(20, 6);
        if let Some(editor) = editor {
            tui.base().set_focus(Some(editor.rc()));
        }
        tui.start();
        tui
    }

    #[test]
    fn focus_on_already_focused_overlay_bumps_visual_order() {
        let editor = Focusable::new("EDITOR");
        let mut tui = small_harness(Some(&editor));
        let a = tui
            .base()
            .show_overlay(static_overlay(&["A"]), Some(corner(true)));
        tui.base()
            .show_overlay(static_overlay(&["B"]), Some(corner(true)));
        a.focus();
        tui.base()
            .show_overlay(static_overlay(&["C"]), Some(corner(true)));
        tui.render_and_flush();
        assert_eq!(first_char(&tui), Some('C'));
        a.focus();
        tui.render_and_flush();
        assert_eq!(first_char(&tui), Some('A'));
        assert!(a.is_focused());
        tui.stop();
    }

    #[test]
    fn default_rendering_order_for_overlapping_overlays_follows_creation_order() {
        let mut tui = small_harness(None);
        tui.base()
            .show_overlay(static_overlay(&["A"]), Some(corner(true)));
        tui.base()
            .show_overlay(static_overlay(&["B"]), Some(corner(true)));
        tui.render_and_flush();
        assert_eq!(first_char(&tui), Some('B'));
        tui.stop();
    }

    #[test]
    fn focus_on_lower_overlay_renders_it_on_top() {
        let mut tui = small_harness(None);
        let lower = tui
            .base()
            .show_overlay(static_overlay(&["A"]), Some(corner(true)));
        tui.base()
            .show_overlay(static_overlay(&["B"]), Some(corner(true)));
        tui.render_and_flush();
        assert_eq!(first_char(&tui), Some('B'));
        lower.focus();
        tui.render_and_flush();
        assert_eq!(first_char(&tui), Some('A'));
        tui.stop();
    }

    #[test]
    fn focusing_middle_overlay_places_it_on_top_while_preserving_others_relative_order() {
        let mut tui = small_harness(None);
        tui.base()
            .show_overlay(static_overlay(&["A"]), Some(corner(true)));
        let middle = tui
            .base()
            .show_overlay(static_overlay(&["B"]), Some(corner(true)));
        let top = tui
            .base()
            .show_overlay(static_overlay(&["C"]), Some(corner(true)));
        tui.render_and_flush();
        assert_eq!(first_char(&tui), Some('C'));
        middle.focus();
        tui.render_and_flush();
        assert_eq!(first_char(&tui), Some('B'));
        middle.hide();
        tui.render_and_flush();
        assert_eq!(first_char(&tui), Some('C'));
        top.hide();
        tui.render_and_flush();
        assert_eq!(first_char(&tui), Some('A'));
        tui.stop();
    }

    #[test]
    fn capturing_overlay_hidden_and_shown_again_renders_on_top_after_unhide() {
        let mut tui = small_harness(None);
        tui.base()
            .show_overlay(static_overlay(&["A"]), Some(corner(true)));
        let capturing = tui
            .base()
            .show_overlay(static_overlay(&["B"]), Some(corner(false)));
        tui.render_and_flush();
        assert_eq!(first_char(&tui), Some('B'));
        capturing.set_hidden(true);
        tui.base()
            .show_overlay(static_overlay(&["C"]), Some(corner(true)));
        tui.render_and_flush();
        assert_eq!(first_char(&tui), Some('C'));
        capturing.set_hidden(false);
        tui.render_and_flush();
        assert_eq!(first_char(&tui), Some('B'));
        tui.stop();
    }

    #[test]
    fn unfocus_does_not_change_visual_order_until_another_overlay_is_focused() {
        let editor = Focusable::new("EDITOR");
        let mut tui = small_harness(Some(&editor));
        let a = tui
            .base()
            .show_overlay(static_overlay(&["A"]), Some(corner(true)));
        let b = tui
            .base()
            .show_overlay(static_overlay(&["B"]), Some(corner(true)));
        tui.render_and_flush();
        assert_eq!(first_char(&tui), Some('B'));
        a.focus();
        tui.render_and_flush();
        assert_eq!(first_char(&tui), Some('A'));
        a.unfocus(None);
        tui.render_and_flush();
        assert_eq!(first_char(&tui), Some('A'));
        b.focus();
        tui.render_and_flush();
        assert_eq!(first_char(&tui), Some('B'));
        tui.stop();
    }

    // ---------- tui-alt-screen.test.ts: transcript search ----------

    fn segments(
        matches: &[maho_tui::alt_screen_search::AltScreenSearchMatch],
    ) -> Vec<Vec<(usize, usize, usize)>> {
        matches
            .iter()
            .map(|m| {
                m.segments
                    .iter()
                    .map(|segment| (segment.row, segment.start_col, segment.end_col))
                    .collect()
            })
            .collect()
    }

    fn strings(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    #[test]
    fn searches_normalized_rendered_transcript_text_across_rows() {
        let matches =
            find_alt_screen_search_matches(&strings(&["alpha QUICK", "brown fox"]), "quick brown");
        assert_eq!(segments(&matches), vec![vec![(0, 6, 11), (1, 0, 5)]]);
    }

    #[test]
    fn maps_normalized_ascii_and_unicode_search_matches_back_to_rendered_columns() {
        let matches = find_alt_screen_search_matches(
            &strings(&["\x1b[31mfoo  bar\x1b[0m", "A界🙂éZ"]),
            "oo   bar\nA界🙂é",
        );
        assert_eq!(
            segments(&matches),
            vec![vec![(0, 1, 3), (0, 5, 8), (1, 0, 6)]]
        );
    }

    /// senpi also asserts the cached result is the identical array (`===`); the Rust index
    /// reports an unchanged search with `changed == false` and lets the caller keep its matches.
    #[test]
    fn reuses_indexed_transcript_matches_until_the_query_or_rendered_lines_change() {
        let mut index = AltScreenSearchIndex::new();
        let lines = strings(&["alpha needle", "omega"]);
        let initial = index.search(&lines, "needle");
        assert!(initial.changed);
        assert_eq!(initial.matches.len(), 1);

        let cached = index.search(&lines, "needle");
        assert!(!cached.changed);

        let changed_query = index.search(&lines, "omega");
        assert!(changed_query.changed);
        assert_eq!(segments(&changed_query.matches[..1]), vec![vec![(1, 0, 5)]]);

        let changed_lines = index.search(&strings(&["alpha needle", "no match"]), "omega");
        assert!(changed_lines.changed);
        assert!(changed_lines.matches.is_empty());
    }
}
