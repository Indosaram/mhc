//! The vt100-backed VirtualTerminal must serialize exactly what senpi's xterm-backed one does.
//! Replays tools/golden/cases/vterm-sgr-basic.json and compares with its generated fixture.

use maho_test_support::golden::{fixture_path, workspace_root};
use maho_test_support::vterm::{Screen, VirtualTerminal};

#[derive(serde::Deserialize)]
struct ScreenCase {
    cols: u16,
    rows: u16,
    writes: Vec<String>,
}

fn load_case(name: &str) -> ScreenCase {
    let path = workspace_root().join(format!("tools/golden/cases/{name}.json"));
    let text = std::fs::read_to_string(&path).expect("case file");
    serde_json::from_str(&text).expect("case json")
}

#[test]
fn vt100_screen_matches_senpi_xterm_fixture_cell_for_cell() {
    let case = load_case("vterm-sgr-basic");
    let mut term = VirtualTerminal::new(case.cols, case.rows);
    for chunk in &case.writes {
        term.write(chunk);
    }
    let fixture = std::fs::read_to_string(fixture_path(
        "maho-test-support",
        &format!("vterm-sgr-basic.{}x{}.json", case.cols, case.rows),
    ))
    .expect("fixture");
    let expected: Screen = serde_json::from_str(&fixture).expect("fixture json");
    let actual = term.snapshot();
    assert_eq!(actual.viewport, expected.viewport);
    assert_eq!(actual.cursor, expected.cursor);
    for (row, (a, e)) in actual.cells.iter().zip(&expected.cells).enumerate() {
        assert_eq!(a, e, "row {row}");
    }
    assert_eq!(actual, expected);
}

#[test]
fn terminal_helpers_emit_the_same_sequences_as_senpi() {
    let mut term = VirtualTerminal::new(10, 3);
    term.start();
    assert!(term.bracketed_paste());
    term.write("abc\r\ndef");
    term.move_by(-1);
    term.clear_line();
    // Cursor is at column 3 of row 0, so erase-to-end-of-line keeps "abc".
    assert_eq!(term.viewport(), vec!["abc", "def", ""]);
    term.write("\r");
    term.clear_line();
    assert_eq!(term.viewport(), vec!["", "def", ""]);
    term.hide_cursor();
    assert!(term.cursor_hidden());
    term.show_cursor();
    term.clear_screen();
    assert_eq!(term.viewport(), vec!["", "", ""]);
    assert_eq!(term.cursor_position().x, 0);
    term.stop();
    assert!(!term.bracketed_paste());
    assert!(term.kitty_protocol_active());
}

#[test]
fn resize_changes_reported_dimensions() {
    let mut term = VirtualTerminal::default();
    assert_eq!((term.columns(), term.rows()), (80, 24));
    term.resize(40, 10);
    let screen = term.snapshot();
    assert_eq!((screen.cols, screen.rows, screen.cells.len()), (40, 10, 10));
    assert!(screen.cells.iter().all(|row| row.len() == 40));
}
