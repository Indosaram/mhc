//! Byte-exact full-screen parity for the main and alternate screen renderers.
//!
//! Each `tools/golden/cases/tui-screen-*.json` case is replayed against the Rust renderer. The
//! `.ansi.json` fixture encodes the byte stream senpi's renderer wrote for the same steps and the
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
        let encoded = std::fs::read_to_string(fixture_path(&format!("{name}.ansi.json")))
            .unwrap_or_else(|error| panic!("{name}.ansi.json: {error}"));
        let raw: String = serde_json::from_str(&encoded).expect("encoded ANSI stream");
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

    type RequestedWidth = Rc<RefCell<Option<usize>>>;
    type StaticOverlayTracked = (Rc<RefCell<dyn Component>>, RequestedWidth);

    /// senpi's `StaticOverlay`: fixed lines, records the width it was rendered at.
    struct StaticOverlay {
        lines: Vec<String>,
        requested_width: RequestedWidth,
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

    fn static_overlay_tracked(lines: &[&str]) -> StaticOverlayTracked {
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

// ---- tui-alt-screen.test.ts: viewport, wheel, scrollbar and keyboard navigation ----
//
// senpi sends input through `VirtualTerminal.sendInput` and awaits `waitForRender`; here each input
// goes straight to `handle_mouse_input`/`handle_terminal_input` and each render is one explicit
// `do_render`. senpi's scrollbar hide timer is `poll_timers(now_ms)` at an explicit clock reading.
mod alt_screen_viewport {
    use super::*;
    use maho_tui::components::h_stack::HStack;
    use maho_tui::components::mouse_region::MouseRegion;
    use maho_tui::components::scroll_view::{
        ScrollView, ScrollViewFollow, ScrollViewOptions, ScrollViewScrollbar,
    };
    use maho_tui::components::stack::{StackChild, StackEntryOptions, StackOptions};
    use maho_tui::components::v_stack::VStack;
    use maho_tui::layout_node::StackBasis;
    use maho_tui::tui::{TuiMouseEventResult, TuiMouseEventType};

    pub(super) struct AltTerminal {
        pub(super) vt: VirtualTerminal,
        pub(super) writes: String,
    }

    impl Terminal for AltTerminal {
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
        fn move_by(&mut self, _lines: i64) {}
        fn hide_cursor(&mut self) {}
        fn show_cursor(&mut self) {}
        fn clear_line(&mut self) {}
        fn clear_from_cursor(&mut self) {}
        fn clear_screen(&mut self) {
            self.write("\x1b[2J\x1b[H");
        }
        fn set_title(&mut self, _title: &str) {}
        fn set_progress(&mut self, _active: bool) {}
    }

    /// A started alt screen (senpi `new TuiAltScreen(terminal)` + `tui.start()` + first render).
    pub(super) struct Alt {
        pub(super) tui: TuiAltScreen,
        pub(super) terminal: AltTerminal,
    }

    impl Alt {
        pub(super) fn new(columns: u16, rows: u16) -> Self {
            let mut vt = VirtualTerminal::new(columns, rows);
            vt.start();
            Self {
                tui: TuiAltScreen::new(),
                terminal: AltTerminal {
                    vt,
                    writes: String::new(),
                },
            }
        }
        pub(super) fn start(&mut self) {
            self.tui.base.prepare_start();
            self.tui
                .before_terminal_start(&mut self.terminal, true, false);
            self.render();
        }
        pub(super) fn render(&mut self) {
            self.tui.do_render(&mut self.terminal);
        }
        /// senpi `terminal.sendInput(mouse); await terminal.waitForRender()`.
        pub(super) fn mouse(&mut self, data: &str) {
            self.tui.handle_mouse_input(data, 0, &mut self.terminal);
            self.render_if_requested();
        }
        /// senpi's render scheduler: paint only when the input requested a frame.
        pub(super) fn render_if_requested(&mut self) {
            if self.tui.base.take_due_render(u64::MAX) {
                self.render();
            }
        }
        pub(super) fn key(&mut self, data: &str) {
            let release = maho_tui::keys::is_key_release(data);
            self.tui.handle_terminal_input(data, release);
            self.render();
        }
        pub(super) fn viewport(&self) -> Vec<String> {
            self.terminal
                .vt
                .viewport()
                .into_iter()
                .map(|line| line.trim_end().to_owned())
                .collect()
        }
    }

    pub(super) fn numbered(count: usize) -> String {
        (1..=count)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(super) fn text(content: &str) -> Rc<RefCell<Text>> {
        Rc::new(RefCell::new(Text::with_padding(content, 0, 0)))
    }

    fn scroll_view(
        content: Rc<RefCell<dyn Component>>,
        follow: ScrollViewFollow,
        primary: bool,
        scrollbar: ScrollViewScrollbar,
    ) -> Rc<RefCell<ScrollView>> {
        ScrollView::new(
            content,
            ScrollViewOptions {
                follow,
                primary,
                scrollbar,
                ..Default::default()
            },
        )
    }

    fn entry(
        component: Rc<RefCell<dyn Component>>,
        basis: StackBasis,
        grow: Option<usize>,
        shrink: Option<usize>,
        min_size: Option<usize>,
    ) -> StackChild {
        StackChild::Entry {
            component,
            options: StackEntryOptions {
                basis: Some(basis),
                grow,
                shrink,
                min_size,
                max_size: None,
            },
        }
    }

    /// senpi `new VStack([{ transcript, basis: 0, grow: 1, minSize: 1 }, { dock, basis: "auto", minSize: 1 }])`.
    fn transcript_over_dock(
        transcript: Rc<RefCell<dyn Component>>,
        dock: Rc<RefCell<dyn Component>>,
    ) -> Rc<RefCell<dyn Component>> {
        Rc::new(RefCell::new(VStack::new(
            vec![
                entry(transcript, StackBasis::Fixed(0), Some(1), None, Some(1)),
                entry(dock, StackBasis::Auto, None, None, Some(1)),
            ],
            StackOptions::default(),
        )))
    }

    #[test]
    fn renders_a_terminal_height_viewport_and_preserves_manual_scroll_position() {
        let mut alt = Alt::new(20, 4);
        let content = text(&numbered(10));
        alt.tui.base.add_child(content.clone());
        alt.start();
        assert_eq!(alt.viewport(), ["line 7", "line 8", "line 9", "line 10"]);
        assert!(alt.tui.is_following_output());
        alt.mouse("\x1b[<64;1;1M");
        assert_eq!(alt.viewport(), ["line 6", "line 7", "line 8", "line 9"]);
        assert_eq!(alt.tui.viewport_top(), 5);
        assert!(!alt.tui.is_following_output());
        content.borrow_mut().set_text(numbered(12));
        alt.render();
        assert_eq!(alt.viewport(), ["line 6", "line 7", "line 8", "line 9"]);
    }

    #[test]
    fn shows_a_clickable_jump_to_end_indicator_on_the_transcripts_last_row_while_scrolled_up() {
        let mut alt = Alt::new(30, 6);
        alt.tui.set_scroll_to_end_indicator(Some(Rc::new(|| {
            "\x1b[7m ↓ Jump to end \x1b[27m".to_owned()
        })));
        let transcript = scroll_view(
            text(&numbered(8)),
            ScrollViewFollow::End,
            true,
            ScrollViewScrollbar::Hidden,
        );
        alt.tui.set_layout_root(Some(transcript_over_dock(
            transcript.clone(),
            text("editor\nfooter"),
        )));
        alt.start();
        assert!(
            !alt.viewport()
                .iter()
                .any(|line| line.contains("Jump to end"))
        );
        alt.mouse("\x1b[<64;1;1M");
        assert!(!transcript.borrow().is_following_end());
        // vt100 rows come back without trailing blanks; senpi's pad to the 30 columns.
        assert_eq!(alt.viewport()[3], "line 7  ↓ Jump to end");
        assert_eq!(alt.viewport()[4], "editor");
        // Pressing next to the label starts a selection instead of jumping.
        alt.mouse("\x1b[<0;2;4M");
        alt.mouse("\x1b[<0;2;4m");
        assert!(!transcript.borrow().is_following_end());
        alt.mouse("\x1b[<0;15;4M");
        alt.mouse("\x1b[<0;15;4m");
        assert!(transcript.borrow().is_following_end());
        assert_eq!(
            alt.viewport(),
            ["line 5", "line 6", "line 7", "line 8", "editor", "footer"]
        );
    }

    #[test]
    fn leaves_the_scrollbar_visible_and_clickable_when_the_jump_to_end_indicator_spans_the_transcript()
     {
        let mut alt = Alt::new(30, 6);
        alt.tui
            .set_scroll_to_end_indicator(Some(Rc::new(|| "↓".repeat(30))));
        let transcript = scroll_view(
            text(&numbered(12)),
            ScrollViewFollow::End,
            true,
            ScrollViewScrollbar::Always,
        );
        alt.tui.set_layout_root(Some(transcript_over_dock(
            transcript.clone(),
            text("editor\nfooter"),
        )));
        alt.start();
        alt.mouse("\x1b[<64;1;1M");
        assert!(!transcript.borrow().is_following_end());
        assert_eq!(
            alt.terminal.vt.viewport()[3],
            format!("{}┃", "↓".repeat(29))
        );
        alt.mouse("\x1b[<0;30;4M");
        alt.mouse("\x1b[<0;30;4m");
        assert!(!transcript.borrow().is_following_end());
    }

    #[test]
    fn never_shows_the_jump_to_end_indicator_for_a_primary_scroll_view_without_follow_end() {
        let mut alt = Alt::new(30, 3);
        alt.tui
            .set_scroll_to_end_indicator(Some(Rc::new(|| " ↓ Jump to end ".to_owned())));
        let transcript = scroll_view(
            text("one\ntwo\nthree\nfour\nfive"),
            ScrollViewFollow::None,
            true,
            ScrollViewScrollbar::Hidden,
        );
        alt.tui.set_layout_root(Some(transcript.clone()));
        alt.start();
        assert!(!transcript.borrow().is_following_end());
        assert!(
            !alt.viewport()
                .iter()
                .any(|line| line.contains("Jump to end"))
        );
    }

    #[test]
    fn keeps_an_explicit_dock_fixed_while_the_transcript_scrolls() {
        let mut alt = Alt::new(20, 6);
        let transcript_text = text(&numbered(8));
        let transcript = scroll_view(
            transcript_text.clone(),
            ScrollViewFollow::End,
            true,
            ScrollViewScrollbar::Hidden,
        );
        let dock: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(VStack::new(
            vec![
                StackChild::Component(text("editor")),
                StackChild::Component(text("footer")),
            ],
            StackOptions::default(),
        )));
        alt.tui
            .set_layout_root(Some(transcript_over_dock(transcript.clone(), dock)));
        alt.start();
        assert_eq!(
            alt.viewport(),
            ["line 5", "line 6", "line 7", "line 8", "editor", "footer"]
        );
        // Wheel over the dock falls back to the primary transcript scroll view.
        alt.mouse("\x1b[<64;1;6M");
        assert_eq!(
            alt.viewport(),
            ["line 4", "line 5", "line 6", "line 7", "editor", "footer"]
        );
        assert!(!transcript.borrow().is_following_end());
        transcript_text.borrow_mut().set_text(numbered(10));
        alt.render();
        assert_eq!(
            alt.viewport(),
            ["line 4", "line 5", "line 6", "line 7", "editor", "footer"]
        );
        alt.tui.scroll_to_bottom();
        alt.render();
        assert_eq!(
            alt.viewport(),
            ["line 7", "line 8", "line 9", "line 10", "editor", "footer"]
        );
    }

    #[test]
    fn routes_wheel_input_to_the_scroll_view_under_the_pointer() {
        let mut alt = Alt::new(20, 4);
        let left = scroll_view(
            text("a1\na2\na3\na4\na5\na6\na7"),
            ScrollViewFollow::End,
            true,
            ScrollViewScrollbar::Hidden,
        );
        let right = scroll_view(
            text("b1\nb2\nb3\nb4\nb5\nb6\nb7"),
            ScrollViewFollow::End,
            false,
            ScrollViewScrollbar::Hidden,
        );
        alt.tui
            .set_layout_root(Some(Rc::new(RefCell::new(HStack::new(
                vec![
                    entry(left.clone(), StackBasis::Fixed(10), None, Some(0), None),
                    entry(right.clone(), StackBasis::Fixed(10), None, Some(0), None),
                ],
                StackOptions::default(),
            )))));
        alt.start();
        alt.mouse("\x1b[<64;15;1M");
        assert_eq!(left.borrow().scroll_top(), 3);
        assert_eq!(right.borrow().scroll_top(), 2);
        assert_eq!(
            alt.viewport(),
            [
                "a4        b3",
                "a5        b4",
                "a6        b5",
                "a7        b6"
            ]
        );
    }

    #[test]
    fn scrolls_faster_while_alt_is_held_during_wheel_input() {
        let mut alt = Alt::new(20, 4);
        alt.tui.base.add_child(text(&numbered(12)));
        alt.start();
        assert_eq!(alt.tui.viewport_top(), 8);
        // Alt sets bit 8 on the wheel button (72 = 64 + 8).
        alt.mouse("\x1b[<72;1;1M");
        assert_eq!(alt.tui.viewport_top(), 3);
    }

    #[test]
    fn reveals_an_auto_scrollbar_when_the_pointer_enters_its_hidden_track() {
        let mut alt = Alt::new(10, 5);
        let view = ScrollView::new(
            text(&numbered(20)),
            ScrollViewOptions {
                primary: true,
                scrollbar: ScrollViewScrollbar::Auto,
                scrollbar_hide_delay_ms: 20,
                ..Default::default()
            },
        );
        alt.tui.set_layout_root(Some(view.clone()));
        alt.start();
        assert!(!view.borrow().is_scrollbar_visible());
        alt.mouse("\x1b[<35;10;3M");
        // The scroll view raises its own render request; the host paints it.
        assert!(view.borrow_mut().take_render_requested());
        alt.render();
        assert!(view.borrow().is_scrollbar_visible());
        assert!(view.borrow().is_scrollbar_active());
        assert!(
            alt.viewport()
                .iter()
                .any(|line| line.contains('│') || line.contains('█'))
        );
        alt.mouse("\x1b[<35;9;3M");
        // senpi waits 40ms past the 20ms hide delay; the poll stands in for that timer.
        let due = view.borrow().hide_due_at_ms().expect("hide timer armed");
        alt.tui.poll_timers(due + 20);
        alt.render();
        assert!(!view.borrow().is_scrollbar_visible());
    }

    #[test]
    fn jumps_to_a_scrollbar_track_position_and_continues_dragging_from_there() {
        let mut alt = Alt::new(10, 10);
        let view = scroll_view(
            text(&numbered(50)),
            ScrollViewFollow::None,
            true,
            ScrollViewScrollbar::Always,
        );
        alt.tui.set_layout_root(Some(view.clone()));
        alt.start();
        assert_eq!(view.borrow().scroll_top(), 0);
        alt.mouse("\x1b[<0;10;6M");
        assert_eq!(view.borrow().scroll_top(), 20);
        alt.mouse("\x1b[<32;10;10M");
        assert_eq!(view.borrow().scroll_top(), 40);
        alt.mouse("\x1b[<0;10;10m");
        assert!(!alt.terminal.writes.contains("\x1b]52;c;"));
    }

    // #9758: wheel line counts can change at runtime; Alt keeps its multiplier.
    #[test]
    fn applies_runtime_wheel_line_count_updates() {
        let mut alt = Alt::new(20, 4);
        alt.tui.set_wheel_scroll_lines(3);
        let deltas = Rc::new(RefCell::new(Vec::new()));
        let seen = deltas.clone();
        alt.tui
            .base
            .add_child(Rc::new(RefCell::new(MouseRegion::new(
                text("wheel target"),
                Box::new(move |event| {
                    if event.event_type != TuiMouseEventType::Wheel {
                        return None;
                    }
                    seen.borrow_mut().push(event.wheel_delta);
                    Some(TuiMouseEventResult {
                        handled: true,
                        ..Default::default()
                    })
                }),
            ))));
        alt.start();
        alt.tui
            .handle_mouse_input("\x1b[<64;1;1M", 0, &mut alt.terminal);
        alt.tui.set_wheel_scroll_lines(2);
        alt.tui
            .handle_mouse_input("\x1b[<65;1;1M", 0, &mut alt.terminal);
        alt.tui
            .handle_mouse_input("\x1b[<72;1;1M", 0, &mut alt.terminal);
        assert_eq!(*deltas.borrow(), [Some(-3), Some(2), Some(-10)]);
    }

    #[test]
    fn chains_unused_wheel_delta_to_an_outer_scroll_view() {
        let mut alt = Alt::new(20, 4);
        alt.tui.set_wheel_scroll_lines(3);
        let inner = scroll_view(
            text("i1\ni2\ni3\ni4\ni5\ni6"),
            ScrollViewFollow::None,
            false,
            ScrollViewScrollbar::Hidden,
        );
        let outer = scroll_view(
            Rc::new(RefCell::new(VStack::new(
                vec![
                    entry(inner.clone(), StackBasis::Fixed(2), None, None, None),
                    StackChild::Component(text("tail1\ntail2\ntail3\ntail4\ntail5")),
                ],
                StackOptions::default(),
            ))),
            ScrollViewFollow::None,
            true,
            ScrollViewScrollbar::Hidden,
        );
        alt.tui.set_layout_root(Some(outer.clone()));
        alt.start();
        alt.mouse("\x1b[<65;1;1M");
        assert_eq!(inner.borrow().scroll_top(), 3);
        assert_eq!(outer.borrow().scroll_top(), 0);
        alt.mouse("\x1b[<65;1;1M");
        assert_eq!(inner.borrow().scroll_top(), 4);
        assert_eq!(outer.borrow().scroll_top(), 2);
    }

    #[test]
    fn supports_configurable_keyboard_viewport_navigation_with_four_rows_of_page_overlap() {
        let mut alt = Alt::new(20, 8);
        alt.tui.base.add_child(text(&numbered(12)));
        alt.start();
        let top = [
            "line 1", "line 2", "line 3", "line 4", "line 5", "line 6", "line 7", "line 8",
        ];
        let bottom = [
            "line 5", "line 6", "line 7", "line 8", "line 9", "line 10", "line 11", "line 12",
        ];
        alt.key("\x1b[57421u");
        alt.key("\x1b[57421;1:3u");
        assert_eq!(alt.viewport(), top);
        alt.key("\x1b[57422u");
        alt.key("\x1b[57422;1:3u");
        assert_eq!(alt.viewport(), bottom);
        alt.key("\x1bOH");
        assert_eq!(alt.viewport(), top);
        alt.key("\x1bOF");
        assert_eq!(alt.viewport(), bottom);
    }
}

// ---- tui-alt-screen.test.ts: links, selection, clipboard, flashes and component mouse routing ----
//
// senpi's `RecordingTerminal` write log is `AltTerminal::writes`; a copy hook's async answer and the
// flash/auto-scroll timers settle through `tick(now_ms)` at explicit clock readings.
mod alt_screen_selection {
    use super::alt_screen_viewport::{Alt, numbered, text};
    use super::*;
    use maho_tui::components::mouse_region::MouseRegion;
    use maho_tui::tui::{TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType};

    fn osc52(text: &str) -> String {
        format!(
            "\x1b]52;c;{}\x07",
            maho_tui::terminal_image::encode_base64(text.as_bytes())
        )
    }

    fn started(columns: u16, rows: u16, content: &str) -> Alt {
        let mut alt = Alt::new(columns, rows);
        alt.tui.base.add_child(text(content));
        alt.start();
        alt
    }

    /// Press at (1,1), drag to (4,2), release with `release_button`.
    fn drag_select(alt: &mut Alt, release_button: u8) {
        alt.mouse("\x1b[<0;1;1M");
        alt.mouse("\x1b[<32;4;2M");
        alt.mouse(&format!("\x1b[<{release_button};4;2m"));
    }

    fn handled() -> Option<TuiMouseEventResult> {
        Some(TuiMouseEventResult {
            handled: true,
            ..Default::default()
        })
    }

    fn recording_copy(alt: &mut Alt, answer: bool) -> Rc<RefCell<Vec<String>>> {
        let copied = Rc::new(RefCell::new(Vec::new()));
        let sink = copied.clone();
        alt.tui
            .set_copy_selection(Some(Rc::new(move |text: String| {
                sink.borrow_mut().push(text);
                let (sender, receiver) = std::sync::mpsc::channel();
                sender.send(answer).expect("receiver alive");
                receiver
            })));
        copied
    }

    /// Settle the copy hook's answer (it resolves on a helper thread) and paint the flash.
    fn settle_copy(alt: &mut Alt) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !alt.tui.tick(1) {
            assert!(
                std::time::Instant::now() < deadline,
                "the copy hook never settled"
            );
            std::thread::yield_now();
        }
        alt.render();
    }

    fn shows(alt: &Alt, needle: &str) -> bool {
        alt.viewport().iter().any(|line| line.contains(needle))
    }

    #[test]
    fn opens_an_osc_8_hyperlink_with_specific_or_generic_release_codes_but_not_on_drag() {
        let url = "https://example.com/path?q=1";
        let bel_url = "https://example.com/bel";
        let emoji_url = "https://example.com/emoji";
        let content = format!(
            "{}\n\x1b]8;;{bel_url}\x07link\x1b]8;;\x07\n{}",
            maho_tui::terminal_image::hyperlink("link", url),
            maho_tui::terminal_image::hyperlink("🙂", emoji_url),
        );
        let mut alt = Alt::new(20, 3);
        let opened = Rc::new(RefCell::new(Vec::new()));
        let sink = opened.clone();
        alt.tui.set_open_url(Some(Rc::new(move |url: &str| {
            sink.borrow_mut().push(url.to_owned())
        })));
        alt.tui.base.add_child(text(&content));
        alt.start();
        alt.mouse("\x1b[<0;2;1M");
        alt.mouse("\x1b[<3;2;1m");
        assert_eq!(*opened.borrow(), [url]);
        alt.mouse("\x1b[<0;2;2M");
        alt.mouse("\x1b[<0;2;2m");
        assert_eq!(*opened.borrow(), [url, bel_url]);
        alt.mouse("\x1b[<0;2;3M");
        alt.mouse("\x1b[<0;2;3m");
        assert_eq!(*opened.borrow(), [url, bel_url, emoji_url]);
        alt.mouse("\x1b[<0;2;1M");
        alt.mouse("\x1b[<32;4;1M");
        alt.mouse("\x1b[<0;4;1m");
        assert_eq!(*opened.borrow(), [url, bel_url, emoji_url]);
    }

    #[test]
    fn selects_visible_text_with_the_mouse_and_copies_it_with_osc_52_after_a_generic_release() {
        let mut alt = started(20, 4, "\x1b[1mal\x1b[0mpha\nbeta\ngamma\ndelta");
        drag_select(&mut alt, 3);
        assert!(
            alt.terminal.writes.contains(&osc52("alpha\nbeta")),
            "{:?}",
            alt.terminal.writes
        );
        assert!(alt.terminal.writes.contains("\x1b[7m"));
        assert!(
            alt.terminal.writes.contains("al\x1b[0m\x1b[7mpha"),
            "selection inverse must be reapplied after a reset inside the selection"
        );
        assert!(shows(&alt, "Copied!"));
    }

    #[test]
    fn uses_an_injected_copy_selection_handler_instead_of_osc_52_and_reports_success() {
        let mut alt = started(20, 4, "alpha\nbeta\ngamma\ndelta");
        let copied = recording_copy(&mut alt, true);
        drag_select(&mut alt, 0);
        settle_copy(&mut alt);
        assert_eq!(*copied.borrow(), ["alpha\nbeta"]);
        assert!(!alt.terminal.writes.contains("\x1b]52;c;"));
        assert!(shows(&alt, "Copied!"));
    }

    #[test]
    fn leaves_selections_visible_without_copying_when_copy_on_select_is_disabled() {
        let mut alt = started(20, 4, "alpha\nbeta\ngamma\ndelta");
        alt.tui.set_copy_on_select(false);
        let copied = recording_copy(&mut alt, true);
        drag_select(&mut alt, 0);
        assert!(copied.borrow().is_empty());
        assert!(alt.tui.has_active_selection());
        assert!(alt.terminal.writes.contains("\x1b[7m"));
        assert!(!shows(&alt, "Copied!"));
    }

    #[test]
    fn copies_an_active_selection_programmatically() {
        let mut alt = started(20, 4, "alpha\nbeta\ngamma\ndelta");
        let copied = recording_copy(&mut alt, true);
        assert!(!alt.tui.has_active_selection());
        assert!(
            !alt.tui
                .copy_active_selection_to_clipboard(&mut alt.terminal)
        );
        drag_select(&mut alt, 0);
        settle_copy(&mut alt);
        assert_eq!(*copied.borrow(), ["alpha\nbeta"]);
        assert!(alt.tui.has_active_selection());
        copied.borrow_mut().clear();
        assert!(
            alt.tui
                .copy_active_selection_to_clipboard(&mut alt.terminal)
        );
        settle_copy(&mut alt);
        assert_eq!(*copied.borrow(), ["alpha\nbeta"]);
        assert!(shows(&alt, "Copied!"));
    }

    #[test]
    fn flashes_an_error_when_the_injected_copy_selection_handler_fails() {
        let mut alt = started(20, 4, "alpha\nbeta\ngamma\ndelta");
        recording_copy(&mut alt, false);
        drag_select(&mut alt, 0);
        settle_copy(&mut alt);
        assert!(shows(&alt, "Copy failed"));
        assert!(!alt.terminal.writes.contains("\x1b]52;c;"));
    }

    #[test]
    fn flashes_a_specific_error_returned_by_the_injected_copy_selection_handler() {
        let mut alt = started(80, 4, "alpha\nbeta\ngamma\ndelta");
        alt.tui.set_copy_on_select(false);
        alt.tui
            .set_copy_selection_with_error(Some(Rc::new(|_text: String| {
                let (sender, receiver) = std::sync::mpsc::channel();
                sender
                    .send(Err("Clipboard unavailable: install wl-clipboard".to_owned()))
                    .expect("receiver alive");
                receiver
            })));
        drag_select(&mut alt, 0);
        // senpi answers `false` once the handler reports an error; here the copy is started and
        // its error settles on the next tick (at 1ms).
        assert!(
            alt.tui
                .copy_active_selection_to_clipboard(&mut alt.terminal)
        );
        settle_copy(&mut alt);
        assert!(shows(&alt, "Clipboard unavailable: install wl-clipboard"));
        assert!(!shows(&alt, "Copy failed"));
        // The error flash lasts senpi's 5000ms from settling at 1ms: up at 5000ms, gone at 5001ms.
        alt.tui.tick(5000);
        alt.render();
        assert!(shows(&alt, "Clipboard unavailable"));
        alt.tui.tick(5001);
        alt.render();
        assert!(!shows(&alt, "Clipboard unavailable"));
    }

    #[test]
    fn does_not_append_whitespace_to_double_click_word_highlighting() {
        let mut alt = started(20, 1, "foo  bar");
        alt.mouse("\x1b[<0;1;1M");
        alt.mouse("\x1b[<0;1;1m");
        alt.mouse("\x1b[<0;3;1M");
        assert!(
            alt.terminal.writes.contains("foo\x1b[27m"),
            "{:?}",
            alt.terminal.writes
        );
    }

    #[test]
    fn coalesces_slash_and_hyphen_separated_segments_for_double_click_word_selection() {
        for (line, needle) in [
            ("extensions/starline/fixed-editor/compositor.ts", "starline"),
            ("earendil-works/pi-tui", "works"),
        ] {
            let mut alt = started(80, 1, line);
            let copied = recording_copy(&mut alt, true);
            let column = line.find(needle).expect("needle") + 1;
            for _ in 0..2 {
                alt.mouse(&format!("\x1b[<0;{column};1M"));
                alt.mouse(&format!("\x1b[<0;{column};1m"));
            }
            assert_eq!(*copied.borrow(), [line]);
        }
    }

    #[test]
    fn highlights_a_complete_whitespace_segment_during_a_word_drag() {
        let mut alt = started(20, 1, "foo  bar");
        alt.mouse("\x1b[<0;1;1M");
        alt.mouse("\x1b[<0;1;1m");
        alt.mouse("\x1b[<0;2;1M");
        alt.mouse("\x1b[<32;4;1M");
        assert!(
            alt.terminal.writes.contains("foo  \x1b[27m"),
            "{:?}",
            alt.terminal.writes
        );
    }

    #[test]
    fn selects_whole_words_on_double_click_extends_word_drags_and_selects_lines_on_triple_click() {
        let mut alt = started(20, 2, "zero alpha beta\ngamma delta");
        for input in [
            "\x1b[<0;6;1M",
            "\x1b[<0;6;1m",
            "\x1b[<0;10;1M",
            "\x1b[<0;10;1m",
        ] {
            alt.mouse(input);
        }
        assert!(alt.terminal.writes.contains(&osc52("alpha")));
        for input in [
            "\x1b[<0;12;1M",
            "\x1b[<0;12;1m",
            "\x1b[<0;14;1M",
            "\x1b[<32;3;2M",
            "\x1b[<0;3;2m",
        ] {
            alt.mouse(input);
        }
        assert!(alt.terminal.writes.contains(&osc52("beta\ngamma")));
        for input in [
            "\x1b[<0;7;2M",
            "\x1b[<0;7;2m",
            "\x1b[<0;9;2M",
            "\x1b[<0;9;2m",
            "\x1b[<0;11;2M",
            "\x1b[<0;11;2m",
        ] {
            alt.mouse(input);
        }
        assert!(alt.terminal.writes.contains(&osc52("gamma delta")));
    }

    #[test]
    fn does_not_repaint_idle_or_zero_width_selections_on_focus_loss() {
        let mut alt = started(20, 4, "alpha\nbeta\ngamma\ndelta");
        let idle = alt.terminal.writes.len();
        alt.mouse("\x1b[O");
        alt.mouse("\x1b[I");
        assert_eq!(alt.terminal.writes.len(), idle);
        // A completed click leaves a zero-width anchor; orphaned drag/release events must not extend it.
        alt.mouse("\x1b[<0;1;1M");
        alt.mouse("\x1b[<0;1;1m");
        alt.mouse("\x1b[<32;4;2M");
        alt.mouse("\x1b[<0;4;2m");
        assert!(!alt.terminal.writes.contains("\x1b]52;c;"));
        // Losing focus after a press without a drag cancels the press without repainting.
        alt.mouse("\x1b[<0;1;3M");
        let pressed = alt.terminal.writes.len();
        alt.mouse("\x1b[O");
        alt.mouse("\x1b[I");
        assert_eq!(alt.terminal.writes.len(), pressed);
        alt.mouse("\x1b[<32;4;2M");
        alt.mouse("\x1b[<0;4;2m");
        assert!(!alt.terminal.writes.contains("\x1b]52;c;"));
        assert!(alt.terminal.writes.contains("\x1b[?1004h"));
        alt.tui.before_terminal_stop(&mut alt.terminal, true);
        assert!(alt.terminal.writes.contains("\x1b[?1004l"));
    }

    #[test]
    fn clears_an_active_visible_selection_on_focus_loss_and_ignores_orphan_events() {
        let mut alt = started(20, 4, "alpha\nbeta\ngamma\ndelta");
        alt.mouse("\x1b[<0;1;1M");
        alt.mouse("\x1b[<32;4;2M");
        let before = alt.terminal.writes.len();
        alt.mouse("\x1b[O");
        alt.mouse("\x1b[I");
        let repaint = alt.terminal.writes[before..].to_owned();
        assert!(
            repaint.contains("alpha") && repaint.contains("beta"),
            "{repaint:?}"
        );
        assert!(!repaint.contains("\x1b[7m"));
        alt.mouse("\x1b[<32;4;2M");
        alt.mouse("\x1b[<0;4;2m");
        assert!(!alt.terminal.writes.contains("\x1b]52;c;"));
    }

    #[test]
    fn retains_a_completed_visible_selection_across_focus_changes() {
        let mut alt = started(20, 4, "alpha\nbeta\ngamma\ndelta");
        alt.tui.set_copy_on_select(false);
        drag_select(&mut alt, 0);
        let completed = alt.terminal.writes.len();
        alt.mouse("\x1b[O");
        alt.mouse("\x1b[I");
        assert_eq!(alt.terminal.writes.len(), completed);
        // senpi `renderNow(true)`: a forced full repaint.
        alt.tui.reset_render_state();
        alt.render();
        let redraw = alt.terminal.writes[completed..].to_owned();
        assert!(
            redraw.contains("alpha") && redraw.contains("beta"),
            "{redraw:?}"
        );
        assert!(redraw.contains("\x1b[7m"));
    }

    #[test]
    fn stacks_flash_messages_and_collapses_them_as_they_expire() {
        let mut alt = started(20, 4, "one\ntwo\nthree\nfour");
        alt.tui.flash("First", Some(80), 0);
        alt.tui.flash("Second", Some(500), 0);
        alt.render();
        let viewport = alt.viewport();
        assert!(viewport[0].ends_with(" First"), "{viewport:?}");
        assert!(viewport[1].ends_with(" Second"), "{viewport:?}");
        assert!(alt.tui.tick(100));
        alt.render();
        let viewport = alt.viewport();
        assert!(viewport[0].ends_with(" Second"), "{viewport:?}");
        assert!(!viewport.iter().any(|line| line.contains("First")));
    }

    #[test]
    fn auto_scrolls_and_extends_a_drag_selection_held_at_the_viewport_edge() {
        let mut alt = started(20, 4, &numbered(10));
        assert_eq!(alt.tui.viewport_top(), 6);
        alt.tui
            .handle_mouse_input("\x1b[<0;1;3M", 0, &mut alt.terminal);
        alt.tui
            .handle_mouse_input("\x1b[<32;1;1M", 0, &mut alt.terminal);
        // senpi holds the drag for 130ms of 50ms auto-scroll intervals.
        for now in [50, 100, 150] {
            alt.tui.tick(now);
        }
        alt.render();
        let top = alt.tui.viewport_top();
        assert!(top < 6, "expected auto-scroll above row 6, got {top}");
        alt.mouse("\x1b[<0;1;1m");
        let mut selected: Vec<String> = (top..8)
            .map(|index| format!("line {}", index + 1))
            .collect();
        selected.push("l".to_owned());
        assert!(
            alt.terminal.writes.contains(&osc52(&selected.join("\n"))),
            "{:?}",
            alt.terminal.writes
        );
    }

    #[test]
    fn snaps_mouse_selection_to_cjk_emoji_and_combining_grapheme_boundaries() {
        let mut alt = started(20, 2, "A界🙂éZ");
        let wide = osc52("界🙂");
        for input in ["\x1b[<0;3;1M", "\x1b[<32;4;1M", "\x1b[<0;4;1m"] {
            alt.mouse(input);
        }
        assert_eq!(alt.terminal.writes.matches(&wide).count(), 1);
        for input in ["\x1b[<0;5;1M", "\x1b[<32;2;1M", "\x1b[<0;2;1m"] {
            alt.mouse(input);
        }
        assert_eq!(alt.terminal.writes.matches(&wide).count(), 2);
        for input in ["\x1b[<0;6;1M", "\x1b[<32;7;1M", "\x1b[<0;7;1m"] {
            alt.mouse(input);
        }
        assert!(alt.terminal.writes.contains(&osc52("éZ")));
    }

    #[test]
    fn ignores_horizontal_trackpad_wheel_events() {
        let mut alt = started(20, 4, &numbered(8));
        alt.mouse("\x1b[<66;1;1M");
        alt.mouse("\x1b[<67;1;1M");
        assert_eq!(alt.tui.viewport_top(), 4);
        assert_eq!(alt.viewport(), ["line 5", "line 6", "line 7", "line 8"]);
    }

    #[test]
    fn dispatches_clicks_to_nested_mouse_regions_without_breaking_drag_selection() {
        let mut alt = Alt::new(20, 2);
        let clicks = Rc::new(RefCell::new(0));
        let counted = clicks.clone();
        alt.tui
            .base
            .add_child(Rc::new(RefCell::new(MouseRegion::new(
                text("clickable\nselectable"),
                Box::new(move |event| {
                    if event.event_type != TuiMouseEventType::Click {
                        return None;
                    }
                    *counted.borrow_mut() += 1;
                    handled()
                }),
            ))));
        alt.start();
        alt.mouse("\x1b[<0;2;1M");
        alt.mouse("\x1b[<0;2;1m");
        assert_eq!(*clicks.borrow(), 1);
        drag_select(&mut alt, 0);
        assert_eq!(*clicks.borrow(), 1);
        assert!(alt.terminal.writes.contains("\x1b]52;c;"));
    }

    /// A mouse-aware control: records event types and answers with `answer`.
    struct Control {
        events: Rc<RefCell<Vec<TuiMouseEventType>>>,
        clicks: Rc<RefCell<Vec<u32>>>,
        renders: Rc<RefCell<usize>>,
        answer: fn(&TuiMouseEvent) -> Option<TuiMouseEventResult>,
    }
    impl Component for Control {
        fn render(&mut self, _width: usize) -> Vec<String> {
            *self.renders.borrow_mut() += 1;
            vec!["control".to_owned()]
        }
        fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
            self.events.borrow_mut().push(event.event_type);
            if event.event_type == TuiMouseEventType::Click {
                self.clicks
                    .borrow_mut()
                    .push(event.click_count.unwrap_or(0));
            }
            (self.answer)(event)
        }
    }

    type ControlEvents = Rc<RefCell<Vec<TuiMouseEventType>>>;
    type ControlHarness = (
        Rc<RefCell<Control>>,
        ControlEvents,
        Rc<RefCell<Vec<u32>>>,
        Rc<RefCell<usize>>,
    );

    fn control(answer: fn(&TuiMouseEvent) -> Option<TuiMouseEventResult>) -> ControlHarness {
        let (events, clicks, renders) = (Rc::default(), Rc::default(), Rc::default());
        let control = Rc::new(RefCell::new(Control {
            events: Rc::clone(&events),
            clicks: Rc::clone(&clicks),
            renders: Rc::clone(&renders),
            answer,
        }));
        (control, events, clicks, renders)
    }

    struct KeyboardOwner;
    impl Component for KeyboardOwner {
        fn render(&mut self, _width: usize) -> Vec<String> {
            Vec::new()
        }
        fn handle_input(&mut self, _data: &str) {}
    }

    #[test]
    fn captures_drag_gestures_for_mouse_aware_components_without_taking_keyboard_focus() {
        let mut alt = Alt::new(20, 2);
        let (component, events, _, _) = control(|event| {
            Some(TuiMouseEventResult {
                handled: true,
                capture: event.event_type == TuiMouseEventType::Press,
                focus: event.event_type == TuiMouseEventType::Press,
                ..Default::default()
            })
        });
        alt.tui.set_layout_root(Some(component));
        alt.start();
        let owner: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(KeyboardOwner));
        alt.tui.base.set_focus(Some(owner.clone()));
        alt.mouse("\x1b[<0;1;1M");
        alt.mouse("\x1b[<32;5;2M");
        alt.mouse("\x1b[<0;5;2m");
        assert_eq!(
            *events.borrow(),
            [
                TuiMouseEventType::Press,
                TuiMouseEventType::Drag,
                TuiMouseEventType::Release
            ]
        );
        // The control has no input handler: capture routes drags to it while the keyboard owner keeps focus.
        assert!(
            alt.tui
                .base
                .get_focused_component()
                .is_some_and(|focused| Rc::ptr_eq(&focused, &owner))
        );
    }

    #[test]
    fn reports_consecutive_click_counts_to_component_owned_controls() {
        let mut alt = Alt::new(20, 1);
        let (component, _, clicks, _) = control(|event| match event.event_type {
            TuiMouseEventType::Press | TuiMouseEventType::Click => handled(),
            _ => None,
        });
        alt.tui.base.add_child(component);
        alt.start();
        for _ in 0..3 {
            alt.mouse("\x1b[<0;1;1M");
            alt.mouse("\x1b[<0;1;1m");
        }
        assert_eq!(*clicks.borrow(), [1, 2, 3]);
    }

    #[test]
    fn does_not_rerender_for_handled_no_op_pointer_motion() {
        let mut alt = Alt::new(20, 2);
        let (component, _, _, renders) = control(|event| {
            (event.event_type == TuiMouseEventType::Move)
                .then(handled)
                .flatten()
        });
        alt.tui.base.add_child(component);
        alt.start();
        let rendered = *renders.borrow();
        let written = alt.terminal.writes.len();
        alt.tui
            .handle_mouse_input("\x1b[<35;1;1M", 0, &mut alt.terminal);
        assert!(!alt.tui.base.render_due(u64::MAX));
        assert_eq!(*renders.borrow(), rendered);
        assert_eq!(alt.terminal.writes.len(), written);
    }

    #[test]
    fn lets_mouse_aware_components_consume_wheel_events_before_viewport_scrolling() {
        let mut alt = Alt::new(20, 3);
        let wheels = Rc::new(RefCell::new(0));
        let counted = wheels.clone();
        alt.tui
            .base
            .add_child(Rc::new(RefCell::new(MouseRegion::new(
                text(&numbered(8)),
                Box::new(move |event| {
                    if event.event_type != TuiMouseEventType::Wheel {
                        return None;
                    }
                    *counted.borrow_mut() += 1;
                    handled()
                }),
            ))));
        alt.start();
        let top = alt.tui.viewport_top();
        alt.mouse("\x1b[<64;1;1M");
        assert_eq!(*wheels.borrow(), 1);
        assert_eq!(alt.tui.viewport_top(), top);
    }
}

// ---- tui-alt-screen.test.ts: find bar, key bindings, overlays, lifecycle, multiplexers and images ----
//
// Process-global state stays per test: environment reads go through `process_env::with_overrides`
// (thread-local), keybinding swaps hold `common::global_keybindings_lock`, and image protocols are
// set on the `TuiAltScreen` instance rather than in the shared capability cache.
mod alt_screen_controls {
    use super::alt_screen_viewport::{Alt, numbered, text};
    use super::*;
    use maho_tui::alt_screen_search::AltScreenSearchComponent;
    use maho_tui::components::h_stack::HStack;
    use maho_tui::components::scroll_view::{ScrollView, ScrollViewFollow, ScrollViewOptions};
    use maho_tui::components::select_list::{SelectItem, SelectList, SelectListTheme};
    use maho_tui::components::stack::{StackChild, StackEntryOptions, StackOptions};
    use maho_tui::components::v_stack::VStack;
    use maho_tui::image_stub::ImageProtocol;
    use maho_tui::keybindings::{
        KeybindingsConfig, KeybindingsManager, Keys, set_keybindings, tui_keybindings,
    };
    use maho_tui::layout_node::StackBasis;
    use maho_tui::process_env::with_overrides;
    use maho_tui::terminal_image::{
        KittyEncodeOptions, KittyImageMetadata, encode_kitty, register_kitty_image_metadata,
    };
    use maho_tui::tui::OverlayOptions;
    use maho_tui::tui_alt_screen::prefers_button_motion_mouse;
    use maho_tui::utils::{strip_terminal_sequences, visible_width};

    const OSC133_ZONE_START: &str = "\x1b]133;A\x07";

    fn started(columns: u16, rows: u16, content: &str) -> Alt {
        let mut alt = Alt::new(columns, rows);
        alt.tui.base.add_child(text(content));
        alt.start();
        alt
    }

    fn sized(
        basis: StackBasis,
        component: Rc<RefCell<dyn Component>>,
        grow: Option<usize>,
        shrink: Option<usize>,
        min_size: Option<usize>,
    ) -> StackChild {
        StackChild::Entry {
            component,
            options: StackEntryOptions {
                basis: Some(basis),
                grow,
                shrink,
                min_size,
                max_size: None,
            },
        }
    }

    fn shows(alt: &Alt, needle: &str) -> bool {
        alt.viewport().iter().any(|line| line.contains(needle))
    }

    /// Terminal column of the byte offset `index` in a rendered (single-width) row.
    fn column(line: &str, index: usize) -> usize {
        line[..index].chars().count()
    }

    /// senpi's `InputOverlay` and the editor stubs: a focusable key sink rendering one row.
    struct KeySink {
        label: &'static str,
        focused: bool,
        inputs: Rc<RefCell<Vec<String>>>,
    }
    impl Component for KeySink {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec![self.label.to_owned()]
        }
        fn handle_input(&mut self, data: &str) {
            self.inputs.borrow_mut().push(data.to_owned());
        }
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

    fn key_sink(label: &'static str) -> (Rc<RefCell<KeySink>>, Rc<RefCell<Vec<String>>>) {
        let inputs = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::new(RefCell::new(KeySink {
            label,
            focused: false,
            inputs: Rc::clone(&inputs),
        }));
        (sink, inputs)
    }

    /// A component that renders fixed lines (senpi's inline `{ render: () => [...] }` objects).
    struct Lines(Vec<String>);
    impl Component for Lines {
        fn render(&mut self, _width: usize) -> Vec<String> {
            self.0.clone()
        }
    }

    fn lines(rows: Vec<String>) -> Rc<RefCell<dyn Component>> {
        Rc::new(RefCell::new(Lines(rows)))
    }

    fn kitty_line(
        image_id: u32,
        columns: u32,
        rows: u32,
        width_px: u32,
        height_px: u32,
        data: &str,
    ) -> String {
        register_kitty_image_metadata(KittyImageMetadata {
            image_id,
            columns,
            rows,
            width_px,
            height_px,
        });
        encode_kitty(
            data,
            &KittyEncodeOptions {
                columns: Some(columns),
                rows: Some(rows),
                image_id: Some(image_id),
                move_cursor: false,
                virtual_placement: false,
            },
        )
    }

    /// Start with `protocol` in place of the probed terminal capability.
    fn start_with_images(alt: &mut Alt, protocol: ImageProtocol) {
        alt.tui.base.prepare_start();
        alt.tui
            .before_terminal_start(&mut alt.terminal, true, false);
        alt.tui.set_image_protocol(protocol);
        alt.render();
    }

    fn primary(content: Rc<RefCell<dyn Component>>) -> Rc<RefCell<ScrollView>> {
        ScrollView::new(
            content,
            ScrollViewOptions {
                primary: true,
                ..Default::default()
            },
        )
    }

    fn with_alt_bindings<R>(bindings: &[(&str, &str)], body: impl FnOnce() -> R) -> R {
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                set_keybindings(KeybindingsManager::new(
                    tui_keybindings(),
                    Default::default(),
                ));
            }
        }
        let _lock = crate::common::global_keybindings_lock();
        let mut user = KeybindingsConfig::new();
        for (id, key) in bindings {
            user.insert((*id).to_owned(), Some(Keys::from(*key)));
        }
        set_keybindings(KeybindingsManager::new(tui_keybindings(), user));
        let _restore = Restore;
        body()
    }

    #[test]
    fn keeps_the_jump_to_end_indicator_centered_as_the_auto_scrollbar_hides_and_reappears() {
        let label = " ↓ Jump to latest message · End ";
        let mut alt = Alt::new(80, 6);
        alt.tui
            .set_scroll_to_end_indicator(Some(Rc::new(move || label.to_owned())));
        let transcript = ScrollView::new(
            text(&numbered(20)),
            ScrollViewOptions {
                follow: ScrollViewFollow::End,
                primary: true,
                scrollbar: maho_tui::components::scroll_view::ScrollViewScrollbar::Auto,
                scrollbar_hide_delay_ms: 0,
                ..Default::default()
            },
        );
        alt.tui.set_layout_root(Some(transcript.clone()));
        alt.start();
        // The virtual terminal trims trailing blanks, so match the label without its own.
        let label_column = |alt: &Alt| {
            let row = alt.viewport()[5].clone();
            row.find(label.trim_end()).map(|index| column(&row, index))
        };
        alt.mouse("\x1b[<64;80;1M");
        alt.render();
        assert!(transcript.borrow().is_scrollbar_visible());
        assert!(!transcript.borrow().is_following_end());
        let scroll_top = transcript.borrow().scroll_top();
        let visible = label_column(&alt);
        alt.mouse("\x1b[<35;79;1M");
        alt.tui.poll_timers(1);
        alt.render();
        assert!(!transcript.borrow().is_scrollbar_visible());
        assert_eq!(transcript.borrow().scroll_top(), scroll_top);
        let hidden = label_column(&alt);
        alt.mouse("\x1b[<35;80;1M");
        alt.render();
        assert!(transcript.borrow().is_scrollbar_visible());
        assert_eq!(transcript.borrow().scroll_top(), scroll_top);
        let revealed = label_column(&alt);
        assert_eq!([visible, hidden, revealed], [Some(24), Some(24), Some(24)]);
    }

    #[test]
    fn invalidates_overlays_with_an_explicit_layout_root() {
        struct Probe(Rc<RefCell<bool>>);
        impl Component for Probe {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["overlay".to_owned()]
            }
            fn invalidate(&mut self) {
                *self.0.borrow_mut() = true;
            }
        }
        let invalidated = Rc::new(RefCell::new(false));
        let mut tui = TuiAltScreen::new();
        tui.set_layout_root(Some(text("root")));
        tui.base
            .show_overlay(Rc::new(RefCell::new(Probe(invalidated.clone()))), None);
        tui.base.invalidate();
        assert!(*invalidated.borrow());
    }

    #[test]
    fn does_not_vertically_redispatch_misses_through_horizontal_layout_containers() {
        let identity: Rc<dyn Fn(&str) -> String> = Rc::new(|text: &str| text.to_owned());
        let mut list = SelectList::new(
            ["a", "b"]
                .into_iter()
                .map(|value| SelectItem {
                    value: value.to_owned(),
                    label: value.to_uppercase(),
                    description: None,
                })
                .collect(),
            2,
            SelectListTheme {
                selected_prefix: identity.clone(),
                selected_text: identity.clone(),
                description: identity.clone(),
                scroll_info: identity.clone(),
                no_match: identity,
                render_row: None,
            },
            Default::default(),
        );
        let selections = Rc::new(RefCell::new(0));
        let counted = selections.clone();
        list.on_select = Some(Box::new(move |_| *counted.borrow_mut() += 1));
        let mut alt = Alt::new(20, 2);
        alt.tui
            .set_layout_root(Some(Rc::new(RefCell::new(HStack::new(
                vec![
                    sized(
                        StackBasis::Fixed(10),
                        Rc::new(RefCell::new(list)),
                        None,
                        None,
                        None,
                    ),
                    sized(StackBasis::Fixed(10), text("plain"), None, None, None),
                ],
                StackOptions::default(),
            )))));
        alt.start();
        alt.mouse("\x1b[<0;15;1M");
        alt.mouse("\x1b[<0;15;1m");
        assert_eq!(*selections.borrow(), 0);
    }

    #[test]
    fn uses_button_motion_tracking_inside_terminal_multiplexers() {
        let start_writes = |multiplexer: bool| {
            let mut alt = Alt::new(80, 24);
            alt.tui
                .before_terminal_start(&mut alt.terminal, true, multiplexer);
            alt.terminal.writes
        };
        let clear = [
            ("TMUX", None),
            ("ZELLIJ", None),
            ("STY", None),
            ("TERM", Some("xterm-256color")),
        ];
        let direct = with_overrides(&clear, prefers_button_motion_mouse);
        assert!(start_writes(direct).contains("\x1b[?1003h"));
        type EnvOverride<'a> = (&'a str, Option<&'a str>);
        type MultiplexerCase<'a> = (&'a str, &'a [EnvOverride<'a>]);
        let multiplexers: [MultiplexerCase<'_>; 5] = [
            (
                "tmux environment",
                &[("TMUX", Some("/tmp/tmux/default,1,0"))],
            ),
            ("tmux TERM", &[("TERM", Some("tmux-256color"))]),
            ("Zellij environment", &[("ZELLIJ", Some("0"))]),
            ("Screen environment", &[("STY", Some("123.session"))]),
            ("Screen TERM", &[("TERM", Some("screen-256color"))]),
        ];
        for (name, environment) in multiplexers {
            let multiplexer = with_overrides(&clear, || {
                with_overrides(environment, prefers_button_motion_mouse)
            });
            let writes = start_writes(multiplexer);
            assert!(
                writes.contains("\x1b[?1002h"),
                "{name} should enable button-motion tracking"
            );
            assert!(
                !writes.contains("\x1b[?1003h"),
                "{name} should not enable all-motion tracking"
            );
            assert!(
                writes.contains("\x1b[?1006h"),
                "{name} should enable SGR mouse encoding"
            );
        }
    }

    /// senpi swaps `process.platform`; the Rust gate is the compile target, so each platform
    /// checks its own half and the TERM_PROGRAM half runs everywhere.
    #[test]
    fn invokes_the_right_click_paste_handler_only_on_windows_outside_vs_code() {
        let mut alt = Alt::new(80, 24);
        let pastes = Rc::new(RefCell::new(0));
        let counted = pastes.clone();
        alt.tui
            .set_on_right_click_paste(Some(Rc::new(move || *counted.borrow_mut() += 1)));
        alt.start();
        with_overrides(&[("TERM_PROGRAM", None)], || {
            alt.mouse("\x1b[<2;1;1M");
            alt.mouse("\x1b[<2;1;1m");
        });
        assert_eq!(*pastes.borrow(), usize::from(cfg!(windows)));
        with_overrides(&[("TERM_PROGRAM", Some("vscode"))], || {
            alt.mouse("\x1b[<2;1;1M")
        });
        assert_eq!(*pastes.borrow(), usize::from(cfg!(windows)));
    }

    #[test]
    fn renders_transcript_search_with_a_muted_placeholder_and_right_aligned_controls() {
        let mut component = AltScreenSearchComponent::new(Box::new(|_: &str| {}), None, false);
        let rendered = component.render(48);
        let lines: Vec<String> = rendered
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect();
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|line| visible_width(line) == 48));
        assert!(
            lines[0].starts_with('┌')
                && lines[0].ends_with('┐')
                && lines[0].trim_matches(['┌', '┐', '─']).is_empty()
        );
        assert!(lines[1].starts_with("│ Find in transcript ") && lines[1].ends_with('│'));
        assert!(rendered[1].contains("\x1b[2m"));
        assert!(lines[2].starts_with("└─") && lines[2].ends_with(" ↑ Shift+Enter · ↓ Enter ─┘"));
        let controls = &lines[2];
        let at =
            |index: usize| component.get_navigation_direction_at(2, column(controls, index) as i64);
        let shift_enter = controls.find("Shift+Enter").expect("previous control");
        assert_eq!(at(controls.find('↑').expect("up arrow")), Some(-1));
        assert_eq!(at(shift_enter + 5), Some(-1));
        assert_eq!(at(controls.find('·').expect("separator")), None);
        assert_eq!(at(controls.find('↓').expect("down arrow")), Some(1));
        assert_eq!(
            at(controls.rfind("Enter").expect("next control") + 2),
            Some(1)
        );

        component.handle_input("n");
        component.set_result(0, 2);
        let populated_render = component.render(48);
        let populated: Vec<String> = populated_render
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect();
        assert!(populated[1].contains('n') && populated[1].contains("1/2"));
        assert!(populated_render[1].contains("\x1b[2m 1/2 \x1b[22m"));
        assert!(
            !populated
                .iter()
                .any(|line| line.contains("Find in transcript"))
        );
    }

    #[test]
    fn navigates_transcript_search_with_hoverable_arrow_buttons_and_toggles_it_with_its_shortcut() {
        let mut alt = Alt::new(120, 6);
        alt.tui.set_search_styles(
            None,
            None,
            Some(Rc::new(|text: &str, hovered: bool| {
                format!(
                    "{}{text}\x1b[49m",
                    if hovered { "\x1b[45m" } else { "\x1b[44m" }
                )
            })),
        );
        alt.tui
            .base
            .add_child(text("needle one\nmiddle\nneedle two\nend"));
        alt.start();
        alt.key("\x1b[102;6u");
        alt.key("needle");
        assert!(shows(&alt, "1/2"));
        assert!(shows(&alt, "↑ Shift+Enter · ↓ Enter"));
        let arrow = |alt: &Alt, find: fn(&str) -> Option<usize>| {
            let viewport = alt.viewport();
            let row = viewport
                .iter()
                .position(|line| line.contains('↑') && line.contains('↓'))
                .expect("controls row");
            let index = find(&viewport[row]).expect("control");
            (column(&viewport[row], index) + 1, row + 1)
        };
        let (x, y) = arrow(&alt, |line| line.rfind("Enter"));
        let before_hover = alt.terminal.writes.len();
        alt.mouse(&format!("\x1b[<35;{x};{y}M"));
        assert!(alt.terminal.writes[before_hover..].contains("\x1b[45m↓ Enter\x1b[49m"));
        alt.mouse(&format!("\x1b[<0;{x};{y}M"));
        alt.render();
        assert!(shows(&alt, "2/2"));
        let (x, y) = arrow(&alt, |line| line.find("Shift+Enter").map(|index| index + 3));
        alt.mouse(&format!("\x1b[<0;{x};{y}M"));
        alt.render();
        assert!(shows(&alt, "1/2"));
        assert!(shows(&alt, "↑ Shift+Enter · ↓ Enter"));
        alt.key("\x1b[102;6u");
        assert!(!shows(&alt, "↑ Shift+Enter · ↓ Enter"));
    }

    #[test]
    fn does_not_treat_transcript_box_drawing_as_search_navigation_buttons() {
        let content = [
            "needle one",
            "middle",
            "needle two",
            "filler",
            "┌────────────────────────────────────────┐",
            "│ box                                    │",
            "└────────────────────────────────────────┘",
            "end",
        ]
        .join("\n");
        let mut alt = started(80, 10, &content);
        alt.key("\x1b[102;6u");
        alt.key("needle");
        assert!(shows(&alt, "1/2") && !shows(&alt, "2/2"));
        let bottom = alt
            .viewport()
            .iter()
            .position(|line| line.starts_with('└'))
            .expect("box bottom");
        alt.mouse(&format!("\x1b[<0;24;{}M", bottom + 1));
        alt.render();
        assert!(shows(&alt, "1/2") && !shows(&alt, "2/2"));
    }

    #[test]
    fn uses_configured_styles_for_current_and_non_current_search_matches() {
        let mut alt = Alt::new(60, 4);
        alt.tui.set_search_styles(
            Some(Rc::new(|text: &str| format!("\x1b[41m{text}\x1b[49m"))),
            Some(Rc::new(|text: &str| format!("\x1b[42m{text}\x1b[49m"))),
            None,
        );
        alt.tui
            .base
            .add_child(text("needle first\nmiddle\nneedle second\nend"));
        alt.start();
        alt.key("\x1b[102;6u");
        alt.key("needle");
        assert!(alt.terminal.writes.contains("\x1b[42mneedle\x1b[49m"));
        assert!(alt.terminal.writes.contains("\x1b[41mneedle\x1b[49m"));
    }

    /// The transcript-over-editor layout senpi's search and modified-key cases share.
    fn transcript_over_editor(
        alt: &mut Alt,
        content: &str,
    ) -> (Rc<RefCell<ScrollView>>, Rc<RefCell<Vec<String>>>) {
        let transcript = ScrollView::new(
            text(content),
            ScrollViewOptions {
                follow: ScrollViewFollow::End,
                primary: true,
                ..Default::default()
            },
        );
        let (editor, inputs) = key_sink("editor");
        alt.tui
            .set_layout_root(Some(Rc::new(RefCell::new(VStack::new(
                vec![
                    sized(
                        StackBasis::Fixed(0),
                        transcript.clone(),
                        Some(1),
                        None,
                        Some(1),
                    ),
                    sized(StackBasis::Fixed(1), editor.clone(), None, Some(0), None),
                ],
                StackOptions::default(),
            )))));
        alt.tui.base.set_focus(Some(editor));
        (transcript, inputs)
    }

    #[test]
    fn searches_the_transcript_with_ctrl_shift_f_and_restores_editor_focus_on_close() {
        let content = (1..=12)
            .map(|line| match line {
                5 => "line 5 needle one".to_owned(),
                10 => "line 10 needle two".to_owned(),
                _ => format!("line {line}"),
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut alt = Alt::new(60, 8);
        let (transcript, editor_inputs) = transcript_over_editor(&mut alt, &content);
        alt.start();
        alt.key("\x1b[102;6u");
        alt.key("needle");
        assert!(!transcript.borrow().is_following_end());
        assert!(shows(&alt, "2/2"));
        assert!(shows(&alt, "↑ Shift+Enter · ↓ Enter"));
        assert!(shows(&alt, "line 10 needle two"));
        assert!(editor_inputs.borrow().is_empty());
        assert!(alt.terminal.writes.contains("\x1b[1;7mneedle\x1b[22;27m"));

        for _ in 0..6 {
            alt.tui
                .handle_mouse_input("\x1b[<64;1;4M", 0, &mut alt.terminal);
        }
        alt.render();
        assert_eq!(transcript.borrow().scroll_top(), 0);
        assert!(
            alt.viewport()
                .iter()
                .any(|line| line.contains("needle") && line.contains("2/2"))
        );

        alt.key("\x07");
        assert!(shows(&alt, "1/2"));
        assert!(shows(&alt, "line 5 needle one"));
        alt.key("\x1b[103;6u");
        assert!(shows(&alt, "2/2"));
        assert!(shows(&alt, "line 10 needle two"));
        alt.key("\x1b");
        alt.key("x");
        assert!(!shows(&alt, "↑ Shift+Enter · ↓ Enter"));
        assert_eq!(*editor_inputs.borrow(), ["x"]);
    }

    #[test]
    fn scrolls_the_transcript_by_half_a_page_with_custom_bindings() {
        with_alt_bindings(
            &[
                ("tui.altScreen.halfPageUp", "ctrl+u"),
                ("tui.altScreen.halfPageDown", "ctrl+d"),
            ],
            || {
                let mut alt = started(20, 10, &numbered(30));
                assert_eq!(alt.tui.viewport_top(), 20);
                alt.key("\x15");
                assert_eq!(alt.tui.viewport_top(), 15);
                alt.key("\x04");
                assert_eq!(alt.tui.viewport_top(), 20);
            },
        );
    }

    #[test]
    fn scrolls_the_transcript_by_one_line_with_custom_bindings() {
        with_alt_bindings(
            &[
                ("tui.altScreen.lineUp", "ctrl+y"),
                ("tui.altScreen.lineDown", "ctrl+e"),
            ],
            || {
                let mut alt = started(20, 10, &numbered(30));
                assert_eq!(alt.tui.viewport_top(), 20);
                alt.key("\x19");
                assert_eq!(alt.tui.viewport_top(), 19);
                alt.key("\x05");
                assert_eq!(alt.tui.viewport_top(), 20);
            },
        );
    }

    #[test]
    fn routes_ctrl_modified_viewport_navigation_to_the_focused_component() {
        let mut alt = Alt::new(20, 6);
        let (transcript, editor_inputs) = transcript_over_editor(&mut alt, &numbered(12));
        alt.start();
        alt.key("\x1bOH");
        assert_eq!(transcript.borrow().scroll_top(), 0);
        assert!(editor_inputs.borrow().is_empty());
        let modified = [
            "\x1b[1;5H",
            "\x1b[1;5F",
            "\x1b[5;5~",
            "\x1b[6;5~",
            "\x1b[57423;5u",
        ];
        for input in modified {
            alt.key(input);
        }
        alt.key("\x1b[57423;5:3u");
        assert_eq!(transcript.borrow().scroll_top(), 0);
        assert_eq!(*editor_inputs.borrow(), modified);
        alt.key("\x1b[6~");
        assert_eq!(transcript.borrow().scroll_top(), 1);
        assert_eq!(*editor_inputs.borrow(), modified);
    }

    #[test]
    fn jumps_between_osc_133_semantic_prompt_markers() {
        let content = (1..=4)
            .flat_map(|message| {
                [
                    format!("{OSC133_ZONE_START}message {message}"),
                    "detail".to_owned(),
                ]
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut alt = started(20, 3, &content);
        assert_eq!(alt.tui.viewport_top(), 5);
        alt.key("\x1b[57419;6u");
        alt.key("\x1b[57419;6:3u");
        assert_eq!(alt.tui.viewport_top(), 4);
        assert_eq!(alt.viewport()[0], "message 3");
        alt.key("\x1b[1;6A");
        assert_eq!(alt.tui.viewport_top(), 2);
        assert_eq!(alt.viewport()[0], "message 2");
        alt.key("\x1b[57420;6u");
        alt.key("\x1b[57420;6:3u");
        assert_eq!(alt.tui.viewport_top(), 4);
        assert_eq!(alt.viewport()[0], "message 3");
        alt.key("\x1b[1;6B");
        assert_eq!(alt.tui.viewport_top(), 5);
        assert_eq!(alt.viewport()[1], "message 4");
        assert!(alt.tui.is_following_output());
    }

    /// senpi's `setCapabilities({ images: "iterm2", ... })` / `resetCapabilitiesCache()` pair is
    /// `with_capabilities`, which pins the capabilities to this test's thread so the alt screen's
    /// save/disable/restore of iTerm2 images and the `Image` fallback never touch the cache that
    /// parallel tests read.
    #[test]
    fn does_not_emit_kitty_graphics_commands_or_osc_133_zones_in_iterm2() {
        use maho_tui::components::image::{Image, ImageOptions, ImageTheme};
        use maho_tui::terminal_capabilities::ImageProtocol as CapabilityImageProtocol;
        use maho_tui::terminal_image::{TerminalCapabilities, get_capabilities, with_capabilities};
        use maho_tui::terminal_text::ImageDimensions;

        let iterm2 = TerminalCapabilities {
            images: Some(CapabilityImageProtocol::Iterm2),
            true_color: true,
            hyperlinks: true,
            ..Default::default()
        };
        let writes = with_capabilities(iterm2, || {
            let mut alt = Alt::new(20, 3);
            alt.tui.base.add_child(lines(vec![
                "\x1b]133;B\x07\x1b]133;C\x07\x1b]133;A\x07content".into(),
            ]));
            alt.tui.base.add_child(Rc::new(RefCell::new(Image::new(
                "AAAA",
                "image/png",
                ImageTheme {
                    fallback_color: Rc::new(|value: &str| value.to_owned()),
                },
                ImageOptions {
                    filename: Some("example.png".into()),
                    ..Default::default()
                },
                Some(ImageDimensions {
                    width_px: 10,
                    height_px: 10,
                }),
            ))));
            alt.start();
            alt.tui.before_terminal_stop(&mut alt.terminal, true);
            alt.tui.after_terminal_stop(&mut alt.terminal, false);
            // Stopping restores the capabilities the alt screen disabled while it was active.
            assert_eq!(
                get_capabilities().images,
                Some(CapabilityImageProtocol::Iterm2)
            );
            alt.terminal.writes
        });
        assert!(!writes.contains("\x1b_G"));
        assert!(!writes.contains("\x1b]133;"));
        assert!(!writes.contains("\x1b]1337;File="));
        assert!(writes.contains("[Image:"));
    }

    #[test]
    fn clears_stale_iterm2_image_placements_when_they_leave_the_viewport() {
        let mut alt = Alt::new(20, 3);
        let image = "\x1b]1337;File=inline=1;width=2;height=auto:AAAA\x07".to_owned();
        alt.tui.base.add_child(lines(vec![
            image,
            String::new(),
            String::new(),
            "after".into(),
            "more".into(),
            "end".into(),
        ]));
        start_with_images(&mut alt, ImageProtocol::Iterm2);
        alt.tui.scroll_to_top();
        alt.render();
        let before = alt.terminal.writes.len();
        alt.tui.scroll_by(1);
        alt.render();
        assert!(alt.terminal.writes[before..].contains("\x1b[2J"));
    }

    #[test]
    fn crops_a_kitty_image_whose_first_line_is_above_the_viewport() {
        let mut alt = Alt::new(20, 3);
        let image = kitty_line(123, 2, 3, 100, 100, "AAAA");
        alt.tui.base.add_child(lines(vec![
            "before".into(),
            image,
            String::new(),
            String::new(),
            "after".into(),
            "end".into(),
        ]));
        alt.start();
        assert_eq!(alt.tui.viewport_top(), 3);
        assert!(
            alt.terminal.writes.contains("i=123") && alt.terminal.writes.contains("y=66,h=34,r=1")
        );
    }

    /// senpi renders an `Image` here; under Kitty that component paints one transmission row plus
    /// blank rows, which this fixture reproduces without touching the shared capability cache.
    #[test]
    fn reuses_moved_kitty_images_without_dropping_hstack_siblings() {
        let mut alt = Alt::new(20, 6);
        let label = Rc::new(RefCell::new(Text::with_padding("left", 0, 0)));
        let header = Rc::new(RefCell::new(Text::with_padding("header", 0, 0)));
        let image = kitty_line(130, 10, 4, 100, 100, &"A".repeat(8192));
        let row: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(HStack::new(
            vec![
                sized(StackBasis::Fixed(10), label.clone(), None, None, None),
                sized(
                    StackBasis::Fixed(10),
                    lines(vec![image, String::new(), String::new(), String::new()]),
                    None,
                    None,
                    None,
                ),
            ],
            StackOptions::default(),
        )));
        alt.tui
            .set_layout_root(Some(Rc::new(RefCell::new(VStack::new(
                vec![
                    sized(StackBasis::Auto, header.clone(), None, None, None),
                    sized(StackBasis::Fixed(4), row, None, None, None),
                ],
                StackOptions::default(),
            )))));
        start_with_images(&mut alt, ImageProtocol::Kitty);
        assert!(alt.terminal.writes.contains("\x1b_Ga=T"));
        let before = alt.terminal.writes.len();
        label.borrow_mut().set_text("changed");
        header.borrow_mut().set_text("header\nsecond");
        alt.render();
        let redraw = alt.terminal.writes[before..].to_owned();
        let placement = redraw.find("\x1b_Ga=p,q=2").expect("placement-only redraw");
        assert!(redraw.contains("\x1b_Ga=d,d=a,q=2\x1b\\"));
        assert!(placement > redraw.find("changed").expect("sibling repainted"));
        assert!(!redraw.contains("\x1b_Ga=T"));
        assert!(
            redraw.len() < 2000,
            "expected placement-only redraw, got {} bytes",
            redraw.len()
        );
        assert!(alt.viewport().iter().any(|line| line == "changed"));
    }

    #[test]
    fn retains_recently_offscreen_kitty_images_for_placement_only_reuse() {
        let mut alt = Alt::new(20, 1);
        let image = kitty_line(321, 2, 1, 100, 50, "AAAA");
        alt.tui
            .set_layout_root(Some(primary(lines(vec![image, "after".into()]))));
        start_with_images(&mut alt, ImageProtocol::Kitty);
        assert!(alt.terminal.writes.contains("\x1b_Ga=T"));
        let before = alt.terminal.writes.len();
        alt.tui.scroll_by(1);
        alt.render();
        alt.tui.scroll_by(-1);
        alt.render();
        let reentry = &alt.terminal.writes[before..];
        assert!(reentry.contains("\x1b_Ga=p,q=2"));
        assert!(!reentry.contains("\x1b_Ga=T"));
        assert!(!reentry.contains("\x1b_Ga=d,d=I,i=321,q=2\x1b\\"));
    }

    #[test]
    fn evicts_the_least_recently_visible_kitty_image_when_the_cache_is_full() {
        let mut alt = Alt::new(20, 1);
        let images: Vec<String> = (0..18)
            .map(|index| kitty_line(500 + index, 2, 1, 100, 50, "AAAA"))
            .collect();
        let count = images.len();
        alt.tui.set_layout_root(Some(primary(lines(images))));
        start_with_images(&mut alt, ImageProtocol::Kitty);
        for _ in 1..count {
            alt.tui.scroll_by(1);
            alt.render();
        }
        assert!(
            alt.terminal
                .writes
                .contains("\x1b_Ga=d,d=I,i=500,q=2\x1b\\")
        );
        let before = alt.terminal.writes.len();
        alt.tui.scroll_to_top();
        alt.render();
        assert!(alt.terminal.writes[before..].contains("\x1b_Ga=T"));
    }

    #[test]
    fn evicts_offscreen_kitty_images_when_decoded_raster_memory_exceeds_the_cache_quota() {
        let mut alt = Alt::new(20, 1);
        let images: Vec<String> = (0..4)
            .map(|index| kitty_line(600 + index, 2, 1, 3840, 2160, "AAAA"))
            .collect();
        let count = images.len();
        alt.tui.set_layout_root(Some(primary(lines(images))));
        start_with_images(&mut alt, ImageProtocol::Kitty);
        for _ in 1..count {
            alt.tui.scroll_by(1);
            alt.render();
        }
        assert!(
            alt.terminal
                .writes
                .contains("\x1b_Ga=d,d=I,i=600,q=2\x1b\\")
        );
    }

    #[test]
    fn restores_keyboard_state_before_leaving_alt_mode_and_prints_the_full_document() {
        let mut alt = Alt::new(20, 3);
        alt.tui
            .base
            .add_child(text("first\nsecond\nthird\nfourth\nfifth\nsixth"));
        alt.tui.base.prepare_start();
        alt.tui
            .before_terminal_start(&mut alt.terminal, true, false);
        // senpi's `terminal.start()` and `terminal.stop()` events, as offsets in the write log.
        let start = alt.terminal.writes.len();
        alt.render();
        alt.tui.before_terminal_stop(&mut alt.terminal, true);
        let stop = alt.terminal.writes.len();
        alt.tui.after_terminal_stop(&mut alt.terminal, false);
        let writes = &alt.terminal.writes;
        assert!(
            writes
                .find("\x1b[?1049h")
                .is_some_and(|index| index < start)
        );
        assert!(writes.find("\x1b[?1006l").is_some_and(|index| index < stop));
        let restore = writes[stop..]
            .find("\x1b[?1049l")
            .expect("main screen restore");
        let restore_write = &writes[stop + restore..];
        for line in ["first", "second", "third", "fourth", "fifth", "sixth"] {
            assert!(
                restore_write.contains(line),
                "{line} missing from {restore_write:?}"
            );
        }
        assert!(restore_write.find("first") < restore_write.find("sixth"));
    }

    #[test]
    fn gives_wheel_and_viewport_keys_to_a_focused_overlay() {
        let mut alt = started(20, 6, &numbered(12));
        let top_before = alt.tui.viewport_top();
        let (overlay, inputs) = key_sink("overlay");
        let handle = alt.tui.base.show_overlay(overlay.clone(), None);
        alt.render();
        assert!(overlay.borrow().focused);
        let wheel = "\x1b[<64;10;3M";
        let keys = ["\x1b[5~", "\x1b[6~", "\x1bOH", "\x1bOF", wheel];
        for key in keys {
            if key == wheel {
                alt.mouse(key);
            } else {
                alt.key(key);
            }
        }
        assert_eq!(*inputs.borrow(), keys);
        assert_eq!(alt.tui.viewport_top(), top_before);
        handle.hide();
        alt.render();
        alt.key("\x1b[5~");
        assert!(alt.tui.viewport_top() < top_before);
    }

    #[test]
    fn keeps_viewport_scrolling_when_an_overlay_is_not_focused() {
        let mut alt = Alt::new(20, 6);
        let (editor, _) = key_sink("editor");
        alt.tui.base.add_child(text(&numbered(12)));
        alt.tui.base.set_focus(Some(editor));
        alt.start();
        let top_before = alt.tui.viewport_top();
        let (hidden, _) = key_sink("overlay");
        alt.tui.base.show_overlay(hidden, None).set_hidden(true);
        let (non_capturing, non_capturing_inputs) = key_sink("overlay");
        alt.tui.base.show_overlay(
            non_capturing.clone(),
            Some(OverlayOptions {
                non_capturing: true,
                ..Default::default()
            }),
        );
        let (unfocused, unfocused_inputs) = key_sink("overlay");
        alt.tui
            .base
            .show_overlay(unfocused.clone(), None)
            .unfocus(None);
        alt.render();
        assert!(!non_capturing.borrow().focused);
        assert!(!unfocused.borrow().focused);
        alt.key("\x1b[5~");
        alt.mouse("\x1b[<64;10;3M");
        assert!(alt.tui.viewport_top() < top_before);
        assert!(non_capturing_inputs.borrow().is_empty());
        assert!(unfocused_inputs.borrow().is_empty());
    }

    #[test]
    fn keeps_viewport_scrolling_while_transcript_search_is_focused() {
        let mut alt = started(20, 6, &numbered(12));
        let top_before = alt.tui.viewport_top();
        alt.key("\x1b[102;6u");
        assert!(shows(&alt, "↑ ↓"));
        alt.key("\x1b[5~");
        alt.mouse("\x1b[<64;1;4M");
        assert!(alt.tui.viewport_top() < top_before);
        assert!(shows(&alt, "↑ ↓"));
    }
}
