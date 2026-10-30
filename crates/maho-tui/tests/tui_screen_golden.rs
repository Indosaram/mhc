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
