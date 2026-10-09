//! Port of the `grok-mermaid` `canvas.ts`.
use std::borrow::Cow;

use super::width::measured;
use super::{Cls, Span};

/// Sentinel occupying the trailing column of a wide glyph. Never emitted: the
/// line builder skips it so a CJK character claims two cells of layout but
/// contributes one character of output.
pub const CONT: &str = "\0";

/// Connection direction bits, combined into a box-drawing glyph by `maskChar`.
pub const U: u8 = 1;
pub const D: u8 = 2;
pub const L: u8 = 4;
pub const R: u8 = 8;

/// Line styles, tracked per cell so crossing edges keep their own stroke.
pub const STY_DOT: u8 = 1;
pub const STY_THICK: u8 = 2;
pub const STY_SOLID: u8 = 4;

/// Result of converting canvas into rendered text lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanvasLines {
    pub plain: Vec<String>,
    pub styled: Vec<Vec<Span>>,
    pub width: usize,
}

/// A grid of cells. Edges accumulate as direction bits rather than glyphs so
/// that crossings and junctions resolve correctly whatever order they are drawn
/// in; `finalizeMask` turns the accumulated bits into characters at the end.
///
/// `occupied` marks cells claimed by a box, which edge bits must not overwrite.
#[derive(Clone, Debug)]
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub ch: Vec<String>,
    pub cls: Vec<Cls>,
    pub mask: Vec<u8>,
    pub style: Vec<u8>,
    pub occupied: Vec<u8>,
    pub cur_style: u8,
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Self {
        let n = w * h;
        Self {
            w,
            h,
            ch: vec![String::from(" "); n],
            cls: vec![Cls::None; n],
            mask: vec![0; n],
            style: vec![0; n],
            occupied: vec![0; n],
            cur_style: STY_SOLID,
        }
    }

    pub fn idx(&self, x: usize, y: usize) -> usize {
        y * self.w + x
    }

    pub fn set(&mut self, x: usize, y: usize, c: &str, cls: Cls) {
        if x >= self.w || y >= self.h {
            return;
        }
        let i = self.idx(x, y);
        self.ch[i] = c.to_owned();
        self.cls[i] = cls;
    }

    /// Accumulate direction bits on a free cell.
    ///
    /// `cls` is the class to claim the cell for; `border` cells are never
    /// reclassified, so a connector meeting a box keeps the box's styling.
    pub fn add_bits(&mut self, x: usize, y: usize, bits: u8, cls: Cls) {
        if x >= self.w || y >= self.h {
            return;
        }
        let i = self.idx(x, y);
        if self.occupied[i] != 0 {
            return;
        }
        self.mask[i] |= bits;
        self.style[i] |= self.cur_style;
        if self.cls[i] != Cls::Border {
            self.cls[i] = cls;
        }
    }

    /// Stamp a finished sub-canvas (a subgraph frame's contents) at an offset.
    pub fn blit(&mut self, sub: &Canvas, ox: usize, oy: usize) {
        for sy in 0..sub.h {
            for sx in 0..sub.w {
                let x = ox + sx;
                let y = oy + sy;
                if x >= self.w || y >= self.h {
                    continue;
                }
                let si = sub.idx(sx, sy);
                let di = self.idx(x, y);
                self.ch[di] = sub.ch[si].clone();
                self.cls[di] = sub.cls[si];
                self.style[di] = sub.style[si];
                self.occupied[di] = 1;
            }
        }
    }

    /// Add direction bits even to an occupied cell, so an edge can meet a border.
    pub fn junction(&mut self, x: usize, y: usize, bits: u8) {
        if x >= self.w || y >= self.h {
            return;
        }
        let i = self.idx(x, y);
        self.mask[i] |= bits;
        if self.cls[i] != Cls::Border {
            self.cls[i] = Cls::Edge;
        }
    }

    pub fn seg_v(&mut self, x: usize, y0: usize, y1: usize) {
        let a = y0.min(y1);
        let b = y0.max(y1);
        for y in a..=b {
            let mut bits = 0u8;
            if y > a {
                bits |= U;
            }
            if y < b {
                bits |= D;
            }
            self.add_bits(x, y, bits, Cls::Edge);
        }
    }

    pub fn seg_h(&mut self, y: usize, x0: usize, x1: usize) {
        let a = x0.min(x1);
        let b = x0.max(x1);
        for x in a..=b {
            let mut bits = 0u8;
            if x > a {
                bits |= L;
            }
            if x < b {
                bits |= R;
            }
            self.add_bits(x, y, bits, Cls::Edge);
        }
    }

    /// Resolve accumulated direction bits into glyphs, honouring line style.
    pub fn finalize_mask(&mut self) {
        for i in 0..self.ch.len() {
            if self.mask[i] != 0 && self.ch[i] == " " {
                let c = mask_char(self.mask[i]);
                self.ch[i] = if self.style[i] == STY_DOT {
                    dotted_char(c).into_owned()
                } else if self.style[i] == STY_THICK {
                    thick_char(c).into_owned()
                } else {
                    c.to_owned()
                };
            }
        }
    }

    /// Mirror top-to-bottom for `BT`. Rows reorder but within-row text does not,
    /// so labels stay readable; box-drawing glyphs flip to match.
    pub fn flip_vertical(&mut self) {
        for y in 0..(self.h / 2) {
            let y2 = self.h - 1 - y;
            for x in 0..self.w {
                let i = self.idx(x, y);
                let j = self.idx(x, y2);
                self.ch.swap(i, j);
                self.cls.swap(i, j);
            }
        }
        for i in 0..self.ch.len() {
            self.ch[i] = flip_glyph_v(&self.ch[i]).into_owned();
        }
    }

    /// Mirror left-to-right for `RL`. Mirroring reverses each row, so after
    /// flipping glyphs each text/label run is reversed back to reading order.
    pub fn flip_horizontal(&mut self) {
        for y in 0..self.h {
            for x in 0..(self.w / 2) {
                let x2 = self.w - 1 - x;
                let i = self.idx(x, y);
                let j = self.idx(x2, y);
                self.ch.swap(i, j);
                self.cls.swap(i, j);
            }
        }
        for i in 0..self.ch.len() {
            self.ch[i] = flip_glyph_h(&self.ch[i]).into_owned();
        }
        for y in 0..self.h {
            let mut x = 0;
            while x < self.w {
                let cls = self.cls[self.idx(x, y)];
                if cls == Cls::Text || cls == Cls::EdgeLabel {
                    let start = self.idx(x, y);
                    while x < self.w && self.cls[self.idx(x, y)] == cls {
                        x += 1;
                    }
                    let end = self.idx(x, y);
                    reverse_slice(&mut self.ch, start, end);
                } else {
                    x += 1;
                }
            }
        }
    }

    /// Group each row into runs of one class, dropping wide-glyph continuations.
    pub fn to_lines(&self) -> CanvasLines {
        let mut plain: Vec<String> = Vec::new();
        let mut styled: Vec<Vec<Span>> = Vec::new();
        let mut width = 0usize;
        for y in 0..self.h {
            // A trailing CONT counts as painted: it is the second cell of a wide
            // glyph, so the row really does reach that column.
            let mut last = 0usize;
            for x in (0..self.w).rev() {
                if self.ch[self.idx(x, y)] != " " {
                    last = x + 1;
                    break;
                }
            }
            width = width.max(last);
            let mut spans: Vec<Span> = Vec::new();
            let mut plain_row = String::new();
            let mut run = String::new();
            let mut run_cls = Cls::None;
            for x in 0..last {
                let i = self.idx(x, y);
                let c = &self.ch[i];
                if c == CONT {
                    continue;
                }
                let cls = self.cls[i];
                plain_row.push_str(c);
                if cls != run_cls && !run.is_empty() {
                    spans.push(Span {
                        text: std::mem::take(&mut run),
                        cls: run_cls,
                    });
                }
                run_cls = cls;
                run.push_str(c);
            }
            if !run.is_empty() {
                spans.push(Span {
                    text: run,
                    cls: run_cls,
                });
            }
            styled.push(spans);
            // Only ASCII spaces, which is all a blank cell ever holds. Trimming `\s`
            // would eat a trailing NBSP that `styled` keeps, desyncing the two.
            let trimmed = plain_row.trim_end_matches(' ');
            plain.push(trimmed.to_owned());
        }
        let mut first = 0usize;
        while first < plain.len() && plain[first].is_empty() {
            first += 1;
        }
        let mut end = plain.len();
        while end > first && plain[end - 1].is_empty() {
            end -= 1;
        }
        CanvasLines {
            plain: plain[first..end].to_vec(),
            styled: styled[first..end].to_vec(),
            width,
        }
    }
}

pub fn reverse_slice(arr: &mut [String], start: usize, end: usize) {
    if start >= end || end > arr.len() {
        return;
    }
    arr[start..end].reverse();
}

/// Paint `text` at `x, y`, one grapheme cluster per cell.
///
/// A wide cluster claims a second cell, marked with `CONT` so the line builder
/// emits one character for it rather than a stray space.
pub fn draw_text(canvas: &mut Canvas, text: &str, x: usize, y: usize, cls: Cls) {
    let mut cur = x;
    for (cluster, cw) in measured(text) {
        if cw == 0 {
            continue;
        }
        canvas.set(cur, y, cluster, cls);
        for k in 1..cw {
            canvas.set(cur + k, y, CONT, cls);
        }
        cur += cw;
    }
}

/// Paint `text` at `x, y`, clearing any edge bits underneath first.
///
/// Used where text sits on top of a drawn line (sequence messages, dividers,
/// compartment rows) and must win over it.
pub fn draw_text_over_edges(canvas: &mut Canvas, text: &str, x: usize, y: usize, cls: Cls) {
    let mut cur = x;
    for (cluster, cw) in measured(text) {
        if cw == 0 {
            continue;
        }
        for k in 0..cw {
            if cur + k < canvas.w && y < canvas.h {
                let idx = canvas.idx(cur + k, y);
                canvas.mask[idx] = 0;
            }
            canvas.set(cur + k, y, if k == 0 { cluster } else { CONT }, cls);
        }
        cur += cw;
    }
}

pub fn mask_char(mask: u8) -> &'static str {
    match mask {
        0 => " ",
        m if m == U || m == D || m == U | D => "│",
        m if m == L || m == R || m == L | R => "─",
        m if m == D | R => "┌",
        m if m == D | L => "┐",
        m if m == U | R => "└",
        m if m == U | L => "┘",
        m if m == U | D | R => "├",
        m if m == U | D | L => "┤",
        m if m == D | L | R => "┬",
        m if m == U | L | R => "┴",
        _ => "┼",
    }
}

pub fn dotted_char<'a>(c: &'a str) -> Cow<'a, str> {
    match c {
        "─" => Cow::Borrowed("╌"),
        "│" => Cow::Borrowed("╎"),
        _ => Cow::Borrowed(c),
    }
}

pub fn thick_char<'a>(c: &'a str) -> Cow<'a, str> {
    match c {
        "─" => Cow::Borrowed("━"),
        "│" => Cow::Borrowed("┃"),
        "┌" => Cow::Borrowed("┏"),
        "┐" => Cow::Borrowed("┓"),
        "└" => Cow::Borrowed("┗"),
        "┘" => Cow::Borrowed("┛"),
        "├" => Cow::Borrowed("┣"),
        "┤" => Cow::Borrowed("┫"),
        "┬" => Cow::Borrowed("┳"),
        "┴" => Cow::Borrowed("┻"),
        "┼" => Cow::Borrowed("╋"),
        _ => Cow::Borrowed(c),
    }
}

pub fn flip_glyph_v<'a>(c: &'a str) -> Cow<'a, str> {
    match c {
        "┌" => Cow::Borrowed("└"),
        "└" => Cow::Borrowed("┌"),
        "┐" => Cow::Borrowed("┘"),
        "┘" => Cow::Borrowed("┐"),
        "┏" => Cow::Borrowed("┗"),
        "┗" => Cow::Borrowed("┏"),
        "┓" => Cow::Borrowed("┛"),
        "┛" => Cow::Borrowed("┓"),
        "╭" => Cow::Borrowed("╰"),
        "╰" => Cow::Borrowed("╭"),
        "╮" => Cow::Borrowed("╯"),
        "╯" => Cow::Borrowed("╮"),
        "┬" => Cow::Borrowed("┴"),
        "┴" => Cow::Borrowed("┬"),
        "┳" => Cow::Borrowed("┻"),
        "┻" => Cow::Borrowed("┳"),
        "▼" => Cow::Borrowed("▲"),
        "▲" => Cow::Borrowed("▼"),
        "▽" => Cow::Borrowed("△"),
        "△" => Cow::Borrowed("▽"),
        _ => Cow::Borrowed(c),
    }
}

pub fn flip_glyph_h<'a>(c: &'a str) -> Cow<'a, str> {
    match c {
        "┌" => Cow::Borrowed("┐"),
        "┐" => Cow::Borrowed("┌"),
        "└" => Cow::Borrowed("┘"),
        "┘" => Cow::Borrowed("└"),
        "┏" => Cow::Borrowed("┓"),
        "┓" => Cow::Borrowed("┏"),
        "┗" => Cow::Borrowed("┛"),
        "┛" => Cow::Borrowed("┗"),
        "╭" => Cow::Borrowed("╮"),
        "╮" => Cow::Borrowed("╭"),
        "╰" => Cow::Borrowed("╯"),
        "╯" => Cow::Borrowed("╰"),
        "├" => Cow::Borrowed("┤"),
        "┤" => Cow::Borrowed("├"),
        "┣" => Cow::Borrowed("┫"),
        "┫" => Cow::Borrowed("┣"),
        "▶" => Cow::Borrowed("◄"),
        "◄" => Cow::Borrowed("▶"),
        "▷" => Cow::Borrowed("◁"),
        "◁" => Cow::Borrowed("▷"),
        _ => Cow::Borrowed(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(CONT, "\0");
        assert_eq!(U, 1);
        assert_eq!(D, 2);
        assert_eq!(L, 4);
        assert_eq!(R, 8);
        assert_eq!(STY_DOT, 1);
        assert_eq!(STY_THICK, 2);
        assert_eq!(STY_SOLID, 4);
    }

    #[test]
    fn test_canvas_initialization() {
        let c = Canvas::new(5, 3);
        assert_eq!(c.w, 5);
        assert_eq!(c.h, 3);
        assert_eq!(c.ch.len(), 15);
        assert_eq!(c.cls.len(), 15);
        assert_eq!(c.mask.len(), 15);
        assert_eq!(c.style.len(), 15);
        assert_eq!(c.occupied.len(), 15);
        assert_eq!(c.cur_style, STY_SOLID);
        assert_eq!(c.idx(2, 1), 7);
    }

    #[test]
    fn test_canvas_set() {
        let mut c = Canvas::new(4, 4);
        c.set(1, 2, "X", Cls::Text);
        let i = c.idx(1, 2);
        assert_eq!(c.ch[i], "X");
        assert_eq!(c.cls[i], Cls::Text);
        // Out of bounds does not panic
        c.set(5, 5, "Y", Cls::Text);
    }

    #[test]
    fn test_add_bits_and_occupied() {
        let mut c = Canvas::new(4, 4);
        c.add_bits(1, 1, U | D, Cls::Edge);
        let i_1_1 = c.idx(1, 1);
        assert_eq!(c.mask[i_1_1], U | D);
        assert_eq!(c.style[i_1_1], STY_SOLID);
        assert_eq!(c.cls[i_1_1], Cls::Edge);

        // Occupied cell is skipped by add_bits
        let i_2_2 = c.idx(2, 2);
        c.occupied[i_2_2] = 1;
        c.add_bits(2, 2, L | R, Cls::Edge);
        assert_eq!(c.mask[i_2_2], 0);

        // Border cls is never reclassified
        let i_1_2 = c.idx(1, 2);
        c.cls[i_1_2] = Cls::Border;
        c.add_bits(1, 2, L, Cls::Edge);
        assert_eq!(c.cls[i_1_2], Cls::Border);
    }

    #[test]
    fn test_junction() {
        let mut c = Canvas::new(4, 4);
        let i_1_1 = c.idx(1, 1);
        c.occupied[i_1_1] = 1;
        c.junction(1, 1, U);
        assert_eq!(c.mask[i_1_1], U);
        assert_eq!(c.cls[i_1_1], Cls::Edge);

        // Preserves border
        let i_2_2 = c.idx(2, 2);
        c.cls[i_2_2] = Cls::Border;
        c.junction(2, 2, D);
        assert_eq!(c.mask[i_2_2], D);
        assert_eq!(c.cls[i_2_2], Cls::Border);
    }

    #[test]
    fn test_blit() {
        let mut sub = Canvas::new(2, 2);
        sub.set(0, 0, "A", Cls::Text);
        sub.set(1, 1, "B", Cls::Edge);
        let sub_1_1 = sub.idx(1, 1);
        sub.style[sub_1_1] = STY_THICK;

        let mut parent = Canvas::new(5, 5);
        parent.blit(&sub, 1, 2);

        let parent_1_2 = parent.idx(1, 2);
        assert_eq!(parent.ch[parent_1_2], "A");
        assert_eq!(parent.cls[parent_1_2], Cls::Text);
        assert_eq!(parent.occupied[parent_1_2], 1);

        let parent_2_3 = parent.idx(2, 3);
        assert_eq!(parent.ch[parent_2_3], "B");
        assert_eq!(parent.cls[parent_2_3], Cls::Edge);
        assert_eq!(parent.style[parent_2_3], STY_THICK);
        assert_eq!(parent.occupied[parent_2_3], 1);
    }

    #[test]
    fn test_seg_h_and_v() {
        let mut c = Canvas::new(5, 5);
        c.seg_h(0, 0, 4);
        c.finalize_mask();
        let lines = c.to_lines();
        assert_eq!(lines.plain, vec!["─────"]);
        assert_eq!(lines.styled, vec![vec![Span {
            text: "─────".to_owned(),
            cls: Cls::Edge,
        }]]);
        assert_eq!(lines.width, 5);

        let mut c2 = Canvas::new(5, 4);
        c2.seg_v(2, 0, 3);
        c2.finalize_mask();
        let lines2 = c2.to_lines();
        assert_eq!(lines2.plain, vec!["  │", "  │", "  │", "  │"]);
        assert_eq!(lines2.width, 3);
    }

    #[test]
    fn test_finalize_mask_styles() {
        let mut c = Canvas::new(3, 3);
        c.cur_style = STY_DOT;
        c.seg_h(0, 0, 2);
        c.cur_style = STY_THICK;
        c.seg_h(1, 0, 2);
        c.cur_style = STY_SOLID;
        c.seg_h(2, 0, 2);
        c.finalize_mask();
        let lines = c.to_lines();
        assert_eq!(lines.plain, vec!["╌╌╌", "━━━", "───"]);
    }

    #[test]
    fn test_flip_vertical() {
        let mut c = Canvas::new(3, 3);
        c.set(0, 0, "┌", Cls::Border);
        c.set(0, 2, "└", Cls::Border);
        c.flip_vertical();
        let c_top = c.idx(0, 0);
        let c_bottom = c.idx(0, 2);
        assert_eq!(c.ch[c_top], "┌");
        assert_eq!(c.ch[c_bottom], "└");

        let mut c2 = Canvas::new(2, 2);
        c2.set(0, 0, "▼", Cls::Edge);
        c2.flip_vertical();
        let c2_bottom = c2.idx(0, 1);
        assert_eq!(c2.ch[c2_bottom], "▲");
    }

    #[test]
    fn test_flip_horizontal() {
        let mut c = Canvas::new(10, 3);
        draw_text(&mut c, "hello", 1, 0, Cls::Text);
        draw_text(&mut c, "world", 2, 1, Cls::EdgeLabel);
        c.flip_horizontal();
        let lines = c.to_lines();
        assert_eq!(lines.plain, vec!["    hello", "   world"]);

        // Wide characters reversal
        let mut c2 = Canvas::new(10, 3);
        draw_text(&mut c2, "日本語", 2, 1, Cls::Text);
        c2.flip_horizontal();
        let lines2 = c2.to_lines();
        assert_eq!(lines2.plain, vec!["  日本語"]);
    }

    #[test]
    fn test_to_lines_trimming_and_spans() {
        let mut c = Canvas::new(6, 4);
        c.set(1, 1, "a", Cls::Text);
        c.set(2, 1, "b", Cls::Edge);
        let lines = c.to_lines();
        assert_eq!(lines.plain, vec![" ab"]);
        assert_eq!(lines.styled, vec![vec![
            Span {
                text: " ".to_owned(),
                cls: Cls::None,
            },
            Span {
                text: "a".to_owned(),
                cls: Cls::Text,
            },
            Span {
                text: "b".to_owned(),
                cls: Cls::Edge,
            },
        ]]);
        assert_eq!(lines.width, 3);
    }

    #[test]
    fn test_reverse_slice() {
        let mut arr = vec!["a".to_owned(), "b".to_owned(), "c".to_owned(), "d".to_owned()];
        reverse_slice(&mut arr, 1, 3);
        assert_eq!(arr, vec!["a", "c", "b", "d"]);
    }

    #[test]
    fn test_draw_text() {
        let mut c = Canvas::new(10, 3);
        draw_text(&mut c, "hi 世界", 1, 1, Cls::Text);
        let lines = c.to_lines();
        assert_eq!(lines.plain, vec![" hi 世界"]);
        assert_eq!(lines.width, 8); // 1 + 2 + 1 + 2 + 2 = 8
    }

    #[test]
    fn test_draw_text_over_edges() {
        let mut c = Canvas::new(10, 3);
        c.seg_h(1, 0, 9);
        draw_text_over_edges(&mut c, "mid", 3, 1, Cls::Text);
        c.finalize_mask();
        let lines = c.to_lines();
        assert_eq!(lines.plain, vec!["───mid────"]);
        assert_eq!(lines.styled, vec![vec![
            Span {
                text: "───".to_owned(),
                cls: Cls::Edge,
            },
            Span {
                text: "mid".to_owned(),
                cls: Cls::Text,
            },
            Span {
                text: "────".to_owned(),
                cls: Cls::Edge,
            },
        ]]);
        assert_eq!(lines.width, 10);
    }

    #[test]
    fn test_mask_char() {
        assert_eq!(mask_char(0), " ");
        assert_eq!(mask_char(U), "│");
        assert_eq!(mask_char(D), "│");
        assert_eq!(mask_char(U | D), "│");
        assert_eq!(mask_char(L), "─");
        assert_eq!(mask_char(R), "─");
        assert_eq!(mask_char(L | R), "─");
        assert_eq!(mask_char(D | R), "┌");
        assert_eq!(mask_char(D | L), "┐");
        assert_eq!(mask_char(U | R), "└");
        assert_eq!(mask_char(U | L), "┘");
        assert_eq!(mask_char(U | D | R), "├");
        assert_eq!(mask_char(U | D | L), "┤");
        assert_eq!(mask_char(D | L | R), "┬");
        assert_eq!(mask_char(U | L | R), "┴");
        assert_eq!(mask_char(U | D | L | R), "┼");
    }

    #[test]
    fn test_dotted_char() {
        assert_eq!(dotted_char("─"), "╌");
        assert_eq!(dotted_char("│"), "╎");
        assert_eq!(dotted_char("┌"), "┌");
    }

    #[test]
    fn test_thick_char() {
        assert_eq!(thick_char("─"), "━");
        assert_eq!(thick_char("│"), "┃");
        assert_eq!(thick_char("┌"), "┏");
        assert_eq!(thick_char("┐"), "┓");
        assert_eq!(thick_char("└"), "┗");
        assert_eq!(thick_char("┘"), "┛");
        assert_eq!(thick_char("├"), "┣");
        assert_eq!(thick_char("┤"), "┫");
        assert_eq!(thick_char("┬"), "┳");
        assert_eq!(thick_char("┴"), "┻");
        assert_eq!(thick_char("┼"), "╋");
        assert_eq!(thick_char("X"), "X");
    }

    #[test]
    fn test_flip_glyph_v() {
        assert_eq!(flip_glyph_v("┌"), "└");
        assert_eq!(flip_glyph_v("└"), "┌");
        assert_eq!(flip_glyph_v("┐"), "┘");
        assert_eq!(flip_glyph_v("┘"), "┐");
        assert_eq!(flip_glyph_v("┏"), "┗");
        assert_eq!(flip_glyph_v("┗"), "┏");
        assert_eq!(flip_glyph_v("┓"), "┛");
        assert_eq!(flip_glyph_v("┛"), "┓");
        assert_eq!(flip_glyph_v("╭"), "╰");
        assert_eq!(flip_glyph_v("╰"), "╭");
        assert_eq!(flip_glyph_v("╮"), "╯");
        assert_eq!(flip_glyph_v("╯"), "╮");
        assert_eq!(flip_glyph_v("┬"), "┴");
        assert_eq!(flip_glyph_v("┴"), "┬");
        assert_eq!(flip_glyph_v("┳"), "┻");
        assert_eq!(flip_glyph_v("┻"), "┳");
        assert_eq!(flip_glyph_v("▼"), "▲");
        assert_eq!(flip_glyph_v("▲"), "▼");
        assert_eq!(flip_glyph_v("▽"), "△");
        assert_eq!(flip_glyph_v("△"), "▽");
        assert_eq!(flip_glyph_v("─"), "─");
    }

    #[test]
    fn test_flip_glyph_h() {
        assert_eq!(flip_glyph_h("┌"), "┐");
        assert_eq!(flip_glyph_h("┐"), "┌");
        assert_eq!(flip_glyph_h("└"), "┘");
        assert_eq!(flip_glyph_h("┘"), "└");
        assert_eq!(flip_glyph_h("┏"), "┓");
        assert_eq!(flip_glyph_h("┓"), "┏");
        assert_eq!(flip_glyph_h("┗"), "┛");
        assert_eq!(flip_glyph_h("┛"), "┗");
        assert_eq!(flip_glyph_h("╭"), "╮");
        assert_eq!(flip_glyph_h("╮"), "╭");
        assert_eq!(flip_glyph_h("╰"), "╯");
        assert_eq!(flip_glyph_h("╯"), "╰");
        assert_eq!(flip_glyph_h("├"), "┤");
        assert_eq!(flip_glyph_h("┤"), "├");
        assert_eq!(flip_glyph_h("┣"), "┫");
        assert_eq!(flip_glyph_h("┫"), "┣");
        assert_eq!(flip_glyph_h("▶"), "◄");
        assert_eq!(flip_glyph_h("◄"), "▶");
        assert_eq!(flip_glyph_h("▷"), "◁");
        assert_eq!(flip_glyph_h("◁"), "▷");
        assert_eq!(flip_glyph_h("│"), "│");
    }
}
