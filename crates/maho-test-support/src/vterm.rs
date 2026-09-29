//! Port of senpi's `packages/tui/test/virtual-terminal.ts` backed by the `vt100` crate.
//!
//! [`VirtualTerminal::snapshot`] serializes the viewport in the same JSON cell format that
//! `tools/golden/run.mjs` produces from xterm.js, so Rust screens compare directly against
//! senpi-generated screen fixtures.

use serde::{Deserialize, Serialize};

/// One screen cell: `ch` is empty for unwritten cells and wide-character continuations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub ch: String,
    /// `"default"`, `"p<index>"` for palette colors, or `"#rrggbb"`.
    pub fg: String,
    pub bg: String,
    /// Subset of bold, dim, italic, underline, inverse in that order.
    pub attrs: Vec<String>,
}

/// Cursor position, zero-based (`x` column, `y` row), as xterm reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
}

/// Full viewport snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    pub cols: u16,
    pub rows: u16,
    pub cursor: Cursor,
    pub viewport: Vec<String>,
    pub cells: Vec<Vec<Cell>>,
}

/// In-memory terminal mirroring senpi's test VirtualTerminal.
pub struct VirtualTerminal {
    parser: vt100::Parser,
    columns: u16,
    rows: u16,
}

impl Default for VirtualTerminal {
    fn default() -> Self {
        Self::new(80, 24)
    }
}

impl VirtualTerminal {
    pub fn new(columns: u16, rows: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, columns, 0),
            columns,
            rows,
        }
    }

    /// Mirrors `start`: enables bracketed paste like senpi's ProcessTerminal.
    pub fn start(&mut self) {
        self.write("\x1b[?2004h");
    }

    /// Mirrors `stop`: disables bracketed paste.
    pub fn stop(&mut self) {
        self.write("\x1b[?2004l");
    }

    pub fn write(&mut self, data: &str) {
        self.parser.process(data.as_bytes());
    }

    pub fn columns(&self) -> u16 {
        self.columns
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// senpi's VirtualTerminal always reports the Kitty protocol as active.
    pub fn kitty_protocol_active(&self) -> bool {
        true
    }

    pub fn move_by(&mut self, lines: i32) {
        match lines.cmp(&0) {
            std::cmp::Ordering::Greater => self.write(&format!("\x1b[{lines}B")),
            std::cmp::Ordering::Less => self.write(&format!("\x1b[{}A", -lines)),
            std::cmp::Ordering::Equal => {}
        }
    }

    pub fn hide_cursor(&mut self) {
        self.write("\x1b[?25l");
    }

    pub fn show_cursor(&mut self) {
        self.write("\x1b[?25h");
    }

    pub fn clear_line(&mut self) {
        self.write("\x1b[K");
    }

    pub fn clear_from_cursor(&mut self) {
        self.write("\x1b[J");
    }

    pub fn clear_screen(&mut self) {
        self.write("\x1b[2J\x1b[H");
    }

    pub fn set_title(&mut self, title: &str) {
        self.write(&format!("\x1b]0;{title}\x07"));
    }

    pub fn resize(&mut self, columns: u16, rows: u16) {
        self.columns = columns;
        self.rows = rows;
        self.parser.screen_mut().set_size(rows, columns);
    }

    pub fn bracketed_paste(&self) -> bool {
        self.parser.screen().bracketed_paste()
    }

    pub fn cursor_hidden(&self) -> bool {
        self.parser.screen().hide_cursor()
    }

    pub fn cursor_position(&self) -> Cursor {
        let (y, x) = self.parser.screen().cursor_position();
        Cursor { x, y }
    }

    /// Visible lines with trailing whitespace trimmed, like xterm's `translateToString(true)`.
    pub fn viewport(&self) -> Vec<String> {
        (0..self.rows).map(|row| self.line_text(row)).collect()
    }

    pub fn snapshot(&self) -> Screen {
        let cells = (0..self.rows)
            .map(|row| (0..self.columns).map(|col| self.cell(row, col)).collect())
            .collect();
        Screen {
            cols: self.columns,
            rows: self.rows,
            cursor: self.cursor_position(),
            viewport: self.viewport(),
            cells,
        }
    }

    fn line_text(&self, row: u16) -> String {
        let screen = self.parser.screen();
        let mut text = String::new();
        for col in 0..self.columns {
            match screen.cell(row, col) {
                Some(cell) if cell.is_wide_continuation() => {}
                Some(cell) if cell.has_contents() => text.push_str(cell.contents()),
                _ => text.push(' '),
            }
        }
        text.trim_end_matches(' ').to_string()
    }

    fn cell(&self, row: u16, col: u16) -> Cell {
        let Some(cell) = self.parser.screen().cell(row, col) else {
            return Cell {
                ch: String::new(),
                fg: "default".to_string(),
                bg: "default".to_string(),
                attrs: Vec::new(),
            };
        };
        let flags = [
            (cell.bold(), "bold"),
            (cell.dim(), "dim"),
            (cell.italic(), "italic"),
            (cell.underline(), "underline"),
            (cell.inverse(), "inverse"),
        ];
        Cell {
            ch: cell.contents().to_string(),
            fg: color_name(cell.fgcolor()),
            bg: color_name(cell.bgcolor()),
            attrs: flags
                .iter()
                .filter(|(on, _)| *on)
                .map(|(_, name)| (*name).to_string())
                .collect(),
        }
    }
}

fn color_name(color: vt100::Color) -> String {
    match color {
        vt100::Color::Default => "default".to_string(),
        vt100::Color::Idx(index) => format!("p{index}"),
        vt100::Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}
