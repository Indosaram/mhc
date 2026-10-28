//! The framed read-only detail viewer (spec 7).

use crate::text::{fit, sanitize};

/// Scroll step of the mouse wheel.
pub const WHEEL_STEP: usize = 3;
/// Scroll step of PageUp/PageDown.
pub const PAGE_STEP: usize = 10;
/// Frame rows: top border, title, hint, bottom border.
pub const CHROME_ROWS: usize = 4;

/// A body row with its style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerRow {
    pub text: String,
    pub error: bool,
}

impl ViewerRow {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: true,
        }
    }
}

/// What a key did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyOutcome {
    Close,
    Scrolled,
    Ignored,
}

/// The viewer state.
#[derive(Clone, Debug)]
pub struct Viewer {
    pub title: String,
    pub rows: Vec<ViewerRow>,
    pub scroll: usize,
    /// Terminal height in rows.
    pub terminal_rows: usize,
}

/// Total popup height for a terminal of `terminal_rows`: about 72 %, at least 5, at most the
/// terminal.
pub fn popup_height(terminal_rows: usize) -> usize {
    let target = terminal_rows * 72 / 100;
    target.max(5).min(terminal_rows)
}

impl Viewer {
    pub fn new(title: impl Into<String>, rows: Vec<ViewerRow>, terminal_rows: usize) -> Self {
        Self {
            title: sanitize(&title.into()),
            rows,
            scroll: 0,
            terminal_rows,
        }
    }

    pub fn height(&self) -> usize {
        popup_height(self.terminal_rows)
    }

    pub fn body_rows(&self) -> usize {
        self.height().saturating_sub(CHROME_ROWS)
    }

    pub fn max_scroll(&self) -> usize {
        self.rows.len().saturating_sub(self.body_rows())
    }

    /// Sets the scroll offset clamped to the content.
    pub fn scroll_to(&mut self, offset: usize) {
        self.scroll = offset.min(self.max_scroll());
    }

    fn scroll_by(&mut self, delta: isize) {
        let next = self.scroll.saturating_add_signed(delta);
        self.scroll_to(next);
    }

    /// Handles terminal input (keys and SGR mouse wheel reports).
    pub fn handle_input(&mut self, data: &str) -> KeyOutcome {
        match data {
            "\x1b" | "q" | "\r" | "\n" | "\x1b[27u" => KeyOutcome::Close,
            "\x1b[B" | "\x1bOB" | "j" => self.scrolled(1),
            "\x1b[A" | "\x1bOA" | "k" => self.scrolled(-1),
            "\x1b[6~" => self.scrolled(PAGE_STEP as isize),
            "\x1b[5~" => self.scrolled(-(PAGE_STEP as isize)),
            other => match parse_wheel(other) {
                Some(down) => self.scrolled(if down {
                    WHEEL_STEP as isize
                } else {
                    -(WHEEL_STEP as isize)
                }),
                None => KeyOutcome::Ignored,
            },
        }
    }

    fn scrolled(&mut self, delta: isize) -> KeyOutcome {
        self.scroll_by(delta);
        KeyOutcome::Scrolled
    }

    /// Renders the framed popup at `width` columns: top border, title, body, hint, bottom border.
    pub fn render(&self, width: usize, paint_error: &dyn Fn(&str) -> String) -> Vec<String> {
        let inner = width.saturating_sub(4);
        let line = |content: String| format!("│ {} │", fit(&content, inner));
        let mut out = Vec::with_capacity(self.height());
        out.push(format!("┌{}┐", "─".repeat(width.saturating_sub(2))));
        out.push(line(self.title.clone()));
        let body = self.body_rows();
        for index in 0..body {
            let content = match self.rows.get(self.scroll + index) {
                Some(row) => {
                    let text = sanitize(&row.text);
                    if row.error { paint_error(&text) } else { text }
                }
                None => String::new(),
            };
            out.push(line(content));
        }
        let hint = if self.rows.len() > body {
            let start = if self.rows.is_empty() {
                0
            } else {
                self.scroll + 1
            };
            let end = (self.scroll + body).min(self.rows.len());
            format!("{start}-{end}/{}  q close", self.rows.len())
        } else {
            "q close".into()
        };
        out.push(line(hint));
        out.push(format!("└{}┘", "─".repeat(width.saturating_sub(2))));
        out
    }
}

/// Parses an SGR mouse wheel report (`ESC [ < 64;x;y M` up, `65` down).
fn parse_wheel(data: &str) -> Option<bool> {
    let rest = data.strip_prefix("\x1b[<")?;
    let body = rest.strip_suffix('M').or_else(|| rest.strip_suffix('m'))?;
    let code: u32 = body.split(';').next()?.parse().ok()?;
    match code & !0b0001_1100 {
        64 => Some(false),
        65 => Some(true),
        _ => None,
    }
}

/// Wraps the goal objective at 48 columns.
pub fn goal_rows(objective: &str) -> Vec<ViewerRow> {
    crate::text::wrap(&sanitize(objective), 48)
        .into_iter()
        .map(ViewerRow::plain)
        .collect()
}
