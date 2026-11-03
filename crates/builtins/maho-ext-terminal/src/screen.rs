//! Port of pi-pty `screen.ts` (`TerminalScreen`) over a vt100 parser.
//!
//! Upstream runs every mutation (feed, resize, backlog replay) through one FIFO queue because
//! xterm parses asynchronously; vt100 parses synchronously under the runtime's screen lock, so each
//! operation completes in call order and the queue, its coalescing and its settlers have nothing
//! left to order. What remains is the operation history: every fed chunk is kept (sanitized) up to
//! the replay budget fixed at construction, and a resize rebuilds the terminal at the new geometry
//! and replays that history, independent of the session's log buffer and its read cursor.
use crate::runtime_session::TerminalScreenSnapshot;
use crate::screen_text::{
    normalize_dimension, normalize_replay_history_length, normalize_scrollback, sanitize_string,
    sanitized_tail,
};
use std::collections::VecDeque;

const DEFAULT_COLS: usize = 80;
const DEFAULT_ROWS: usize = 24;

pub struct TerminalScreen {
    parser: vt100::Parser,
    history: VecDeque<String>,
    /// UTF-16 units held in `history` (upstream string lengths).
    history_length: usize,
    max_replay_history_length: usize,
    scrollback: usize,
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

/// `normalize_dimension` returns a value clamped to 1..=10_000, which always fits a u16.
fn dimension(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

impl TerminalScreen {
    /// `new TerminalScreen({ cols, rows, scrollback })`; a zero dimension takes the default.
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Self {
        let cols = normalize_dimension(usize::from(cols), DEFAULT_COLS);
        let rows = normalize_dimension(usize::from(rows), DEFAULT_ROWS);
        let scrollback = normalize_scrollback(scrollback);
        Self {
            parser: vt100::Parser::new(dimension(rows), dimension(cols), scrollback),
            history: VecDeque::new(),
            history_length: 0,
            max_replay_history_length: normalize_replay_history_length(cols, rows, scrollback),
            scrollback,
        }
    }

    /// `feed(data)`: keep the sanitized chunk for resize replay, then parse it.
    pub fn feed(&mut self, payload: &str) {
        let sanitized = sanitize_string(payload);
        // Replace \u{fffd} with \u{E000} before feeding
        let for_parser = sanitized.replace('\u{fffd}', "\u{E000}");
        self.parser.process(for_parser.as_bytes());
        if !sanitized.is_empty() {
            self.append_history(sanitized);
        }
    }

    /// `resize(cols, rows)`: a fresh terminal at the new geometry replays the retained history, so
    /// wrapped lines reflow; a zero dimension keeps the current one.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (current_rows, current_cols) = self.parser.screen().size();
        let cols = normalize_dimension(usize::from(cols), usize::from(current_cols));
        let rows = normalize_dimension(usize::from(rows), usize::from(current_rows));
        let mut parser = vt100::Parser::new(dimension(rows), dimension(cols), self.scrollback);
        for chunk in &self.history {
            parser.process(chunk.as_bytes());
        }
        self.parser = parser;
    }

    /// `snapshot()`: visible rows, the retained scrollback (oldest first) and the cursor as
    /// `(row, col)`.
    pub fn snapshot(&mut self) -> TerminalScreenSnapshot {
        let screen = self.parser.screen_mut();
        let (rows, cols) = screen.size();
        let visible_grid = screen
            .rows(0, cols)
            .map(|row| row.replace('\u{E000}', "\u{fffd}"))
            .collect();
        let cursor = screen.cursor_position();
        screen.set_scrollback(usize::MAX);
        let count = screen.scrollback();
        let mut scrollback = Vec::with_capacity(count);
        for offset in (1..=count).rev() {
            screen.set_scrollback(offset);
            scrollback.push(
                screen
                    .rows(0, cols)
                    .next()
                    .unwrap_or_default()
                    .replace('\u{E000}', "\u{fffd}"),
            );
        }
        screen.set_scrollback(0);
        TerminalScreenSnapshot {
            cols,
            rows,
            visible_grid,
            scrollback,
            cursor,
        }
    }

    fn append_history(&mut self, payload: String) {
        self.history_length += utf16_len(&payload);
        self.history.push_back(payload);
        self.trim_history();
    }

    /// `trimHistory`: drop whole oldest chunks while over budget, then cut a lone oversized chunk
    /// to its sanitized tail.
    fn trim_history(&mut self) {
        while self.history_length > self.max_replay_history_length && self.history.len() > 1 {
            let Some(removed) = self.history.pop_front() else {
                return;
            };
            self.history_length -= utf16_len(&removed);
        }
        if self.history_length <= self.max_replay_history_length {
            return;
        }
        if let Some(only) = self.history.front_mut() {
            *only = sanitized_tail(only, self.max_replay_history_length);
            self.history_length = utf16_len(only);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_ansi_visible_lines_as_plain_grid_text() {
        let mut screen = TerminalScreen::new(10, 3, 10);
        screen.feed("\x1b[31mred\x1b[0m\r\nplain");
        let snapshot = screen.snapshot();
        assert_eq!(snapshot.visible_grid, ["red", "plain", ""]);
        assert_eq!(snapshot.cursor, (1, 5));
    }

    #[test]
    fn tracks_cursor_moves_and_clear_line_sequences() {
        let mut screen = TerminalScreen::new(12, 4, 10);
        screen.feed("alpha\r\nbeta\r\ncharlie");
        screen.feed("\x1b[2;1H\x1b[2Kdone");
        let snapshot = screen.snapshot();
        assert_eq!(snapshot.visible_grid, ["alpha", "done", "charlie", ""]);
        assert_eq!(snapshot.cursor, (1, 4));
    }

    #[test]
    fn reflows_wrapped_lines_when_resized() {
        let mut screen = TerminalScreen::new(6, 4, 10);
        screen.feed("abcdefghi");
        screen.resize(3, 4);
        let snapshot = screen.snapshot();
        assert_eq!(snapshot.cols, 3);
        assert_eq!(snapshot.visible_grid, ["abc", "def", "ghi", ""]);
    }

    #[test]
    fn caps_scrollback_in_snapshots() {
        let mut screen = TerminalScreen::new(8, 2, 3);
        screen.feed("l1\r\nl2\r\nl3\r\nl4\r\nl5\r\nl6");
        let snapshot = screen.snapshot();
        assert_eq!(snapshot.scrollback, ["l2", "l3", "l4"]);
        assert_eq!(snapshot.visible_grid, ["l5", "l6"]);
    }

    #[test]
    fn sanitizes_malformed_utf8_bytes() {
        let mut screen = TerminalScreen::new(20, 2, 10);
        screen.feed(&crate::screen_text::decode_input(&[
            0x6f, 0x6b, 0x20, 0xe2, 0x28, 0xa1,
        ]));
        let row = &screen.snapshot().visible_grid[0];
        assert_eq!(row, "ok \u{fffd}(\u{fffd}");
        assert!(!row.contains('\0'));
    }

    #[test]
    fn orders_resize_replay_between_feeds() {
        let mut screen = TerminalScreen::new(6, 4, 10);
        screen.feed("abcdef");
        screen.resize(3, 4);
        screen.feed("ghi");
        let snapshot = screen.snapshot();
        assert_eq!(snapshot.cols, 3);
        assert_eq!(snapshot.visible_grid, ["abc", "def", "ghi", ""]);
    }

    #[test]
    fn bounds_resize_replay_history_in_long_sessions() {
        let mut screen = TerminalScreen::new(12, 2, 2);
        for index in 0..1200 {
            screen.feed(&format!("line-{index:04}\r\n"));
        }
        assert!(screen.history_length <= 4096);
        assert_eq!(
            screen.history_length,
            screen
                .history
                .iter()
                .map(|chunk| utf16_len(chunk))
                .sum::<usize>()
        );
        screen.resize(12, 2);
        let snapshot = screen.snapshot();
        assert_eq!(snapshot.visible_grid, ["line-1199", ""]);
        assert_eq!(snapshot.scrollback, ["line-1197", "line-1198"]);
    }

    #[test]
    fn trims_an_oversized_chunk_to_the_replay_cap() {
        // A 10x3 screen with 10 scrollback rows has the 4,096-unit minimum budget.
        let mut screen = TerminalScreen::new(10, 3, 10);
        screen.feed(&format!("{}\n", "y".repeat(9_999)));
        assert_eq!(screen.history.len(), 1);
        assert_eq!(screen.history_length, 4096);
        assert!(screen.history[0].ends_with("y\n"));
        // A later chunk evicts the whole trimmed chunk once both no longer fit.
        screen.feed(&"z".repeat(100));
        assert_eq!(screen.history.len(), 1);
        assert_eq!(screen.history_length, 100);
    }

    #[test]
    fn replay_budget_is_fixed_at_construction_and_zero_keeps_dimensions() {
        let mut screen = TerminalScreen::new(10, 3, 10);
        screen.resize(300, 80);
        assert_eq!(screen.max_replay_history_length, 4096);
        screen.resize(0, 0);
        let snapshot = screen.snapshot();
        assert_eq!((snapshot.cols, snapshot.rows), (300, 80));
        let empty = TerminalScreen::new(0, 0, 0).snapshot();
        assert_eq!((empty.cols, empty.rows), (80, 24));
    }

    #[test]
    fn resize_without_history_starts_a_blank_terminal_at_the_new_geometry() {
        let mut screen = TerminalScreen::new(10, 2, 0);
        screen.resize(5, 3);
        let snapshot = screen.snapshot();
        assert_eq!((snapshot.cols, snapshot.rows), (5, 3));
        assert_eq!(snapshot.visible_grid, ["", "", ""]);
        assert_eq!(snapshot.cursor, (0, 0));
    }
}
