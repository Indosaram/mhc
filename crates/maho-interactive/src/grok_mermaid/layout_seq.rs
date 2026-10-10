use super::canvas::{draw_text_over_edges, Canvas, D, L, R, U};
use super::labels::{fit_label, WRAP_WIDTH};
use super::layout::{draw_box, CanvasResult, Placed};
use super::parse::{NoteAnchor, SeqItem, Sequence};
use super::width::string_width;
use super::Cls;

const PAD: usize = 1;
const SEQ_GAP: usize = 5;
const MAX_CANVAS_CELLS: usize = 1 << 21;

fn sat(a: usize, b: usize) -> usize { a.saturating_sub(b) }
fn half(n: usize) -> usize { n / 2 }

fn note_geometry(xs: &[usize], anchor: &NoteAnchor, text_w: usize) -> (usize, usize) {
    match anchor {
        NoteAnchor::Over { from, to } => {
            let center = half(xs[*from] + xs[*to]);
            let w = (xs[*to] - xs[*from] + 5).max(text_w + 2 * PAD + 2);
            (sat(center, half(w)), w)
        }
        NoteAnchor::Left { at } => {
            let w = text_w + 2 * PAD + 2;
            (sat(xs[*at], 2 + w - 1), w)
        }
        NoteAnchor::Right { at } => {
            let w = text_w + 2 * PAD + 2;
            (xs[*at] + 2, w)
        }
    }
}

fn item_text_w(text: Option<&str>) -> usize { text.map_or(0, string_width) }

pub fn layout_sequence(seq: &Sequence) -> CanvasResult {
    let n = seq.labels.len();
    if n == 0 { return None; }
    let labels: Vec<String> = seq.labels.iter().map(|l| fit_label(l, WRAP_WIDTH)).collect();
    let box_w: Vec<usize> = labels.iter().map(|l| string_width(l).max(1) + 2 * PAD + 2).collect();
    let box_h = 3;

    let mut gaps: Vec<usize> = (0..sat(n, 1)).map(|i| {
        SEQ_GAP.max(box_w[i].div_ceil(2) + box_w[i + 1].div_ceil(2) + 1)
    }).collect();
    let mut reqs: Vec<(usize, usize, usize)> = Vec::new();
    for item in &seq.items {
        match item {
            SeqItem::Message { from, to, text, .. } => {
                let tw = item_text_w(text.as_deref());
                if from != to {
                    reqs.push((*from.min(to), *from.max(to), (tw + 2).max(4)));
                } else if *from + 1 < n {
                    reqs.push((*from, *from + 1, 5 + tw + 2));
                }
            }
            SeqItem::Note { anchor, text } => {
                let tw = string_width(text);
                match anchor {
                    NoteAnchor::Over { from, to } if from < to => reqs.push((*from, *to, sat(tw, 1))),
                    NoteAnchor::Over { from, .. } => {
                        let need = (tw + 4).div_ceil(2) + 2;
                        if *from > 0 { reqs.push((*from - 1, *from, need)); }
                        if *from + 1 < n { reqs.push((*from, *from + 1, need)); }
                    }
                    NoteAnchor::Left { at } if *at > 0 => reqs.push((*at - 1, *at, tw + 7)),
                    NoteAnchor::Right { at } if *at + 1 < n => reqs.push((*at, *at + 1, tw + 7)),
                    _ => {}
                }
            }
            SeqItem::Divider { .. } => {}
        }
    }
    reqs.sort_by_key(|(l, r, _)| r - l);
    for (l, r, need) in reqs {
        let cur: usize = gaps[l..r].iter().sum();
        if cur < need { gaps[r - 1] += need - cur; }
    }

    let mut xs = vec![0; n];
    xs[0] = half(box_w[0]);
    for i in 1..n { xs[i] = xs[i - 1] + gaps[i - 1]; }
    let mut canvas_w = xs[n - 1] + box_w[n - 1].div_ceil(2) + 1;
    for item in &seq.items {
        match item {
            SeqItem::Message { from, to, text, .. } if from == to => {
                canvas_w = canvas_w.max(xs[*from] + 5 + item_text_w(text.as_deref()) + 1);
            }
            SeqItem::Note { anchor, text } => {
                let (x, w) = note_geometry(&xs, anchor, string_width(text));
                canvas_w = canvas_w.max(x + w + 1);
            }
            SeqItem::Divider { text } => canvas_w = canvas_w.max(string_width(text) + 4),
            _ => {}
        }
    }
    let mut rows = Vec::with_capacity(seq.items.len());
    let mut y = box_h + 1;
    for item in &seq.items { rows.push(y); y += row_height(item); }
    let bottom_top = y;
    let canvas_h = bottom_top + box_h;
    if canvas_w * canvas_h > MAX_CANVAS_CELLS { return None; }
    let mut canvas = Canvas::new(canvas_w, canvas_h);
    for i in 0..n {
        for by in [0, bottom_top] {
            draw_box(&mut canvas, box_placed(sat(xs[i], half(box_w[i])), by, box_w[i], box_h), &[labels[i].clone()], super::graph::Shape::Rect);
        }
    }
    for (k, item) in seq.items.iter().enumerate() {
        if let SeqItem::Note { anchor, text } = item {
            let (x, w) = note_geometry(&xs, anchor, string_width(text));
            draw_box(&mut canvas, box_placed(x, rows[k], w, 3), std::slice::from_ref(text), super::graph::Shape::Rect);
        }
    }
    for &x in &xs {
        canvas.junction(x, box_h - 1, D);
        canvas.seg_v(x, box_h, bottom_top - 1);
        canvas.junction(x, bottom_top, U);
    }
    for (k, item) in seq.items.iter().enumerate() {
        match item {
            SeqItem::Message { .. } => draw_message(&mut canvas, item, &xs, rows[k]),
            SeqItem::Divider { text } => draw_divider(&mut canvas, text, rows[k], canvas_w),
            SeqItem::Note { .. } => {}
        }
    }
    canvas.finalize_mask();
    Some(canvas)
}

fn row_height(item: &SeqItem) -> usize {
    match item {
        SeqItem::Note { .. } => 4,
        SeqItem::Divider { .. } => 2,
        SeqItem::Message { from, to, text, .. } if from == to => 4,
        SeqItem::Message { text, .. } => if text.is_some() { 3 } else { 2 },
    }
}

fn box_placed(x: usize, y: usize, w: usize, h: usize) -> Placed {
    Placed { x, y, w, h, cx: x + half(w), cy: y + 1, rank: 0 }
}

fn draw_message(canvas: &mut Canvas, item: &SeqItem, xs: &[usize], r: usize) {
    let SeqItem::Message { from, to, text, dashed, head } = item else { return };
    let line_ch = if *dashed { "╌" } else { "─" };
    if from == to {
        let x = xs[*from];
        canvas.junction(x, r, R);
        for dx in 1..=2 { canvas.set(x + dx, r, line_ch, Cls::Edge); }
        canvas.set(x + 3, r, "╮", Cls::Edge); canvas.set(x + 3, r + 1, "│", Cls::Edge);
        canvas.set(x + 1, r + 2, if *head == "cross" { "×" } else { "◄" }, Cls::Edge);
        canvas.set(x + 2, r + 2, line_ch, Cls::Edge); canvas.set(x + 3, r + 2, "╯", Cls::Edge);
        if let Some(text) = text { draw_text_over_edges(canvas, text, x + 5, r + 1, Cls::Text); }
        return;
    }
    let x0 = xs[*from]; let x1 = xs[*to]; let rightward = x1 > x0;
    let arrow_row = if text.is_some() { r + 1 } else { r };
    let lo = x0.min(x1); let hi = x0.max(x1);
    canvas.junction(x0, arrow_row, if rightward { R } else { L });
    for x in lo + 1..hi { canvas.set(x, arrow_row, line_ch, Cls::Edge); }
    let head_ch = if *head == "cross" { "×" } else if rightward { "▶" } else { "◄" };
    canvas.set(if rightward { x1 - 1 } else { x1 + 1 }, arrow_row, head_ch, Cls::Edge);
    if let Some(text) = text {
        let span = hi - lo - 1; let t = fit_label(text, span.max(1));
        draw_text_over_edges(canvas, &t, lo + 1 + half(sat(span, string_width(&t))), r, Cls::Text);
    }
}

fn draw_divider(canvas: &mut Canvas, text: &str, r: usize, canvas_w: usize) {
    for x in 0..canvas_w { canvas.set(x, r, "─", Cls::Edge); }
    draw_text_over_edges(canvas, &format!(" {} ", fit_label(text, sat(canvas_w, 4))), 2, r, Cls::EdgeLabel);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sequence(items: Vec<SeqItem>) -> Sequence {
        let mut s = Sequence::new(); s.participant("A", None); s.participant("B", None); s.items = items; s
    }

    #[test]
    fn note_geometry_covers_each_anchor() {
        let xs = [2, 10];
        assert_eq!(note_geometry(&xs, &NoteAnchor::Over { from: 0, to: 1 }, 3), (0, 13));
        assert_eq!(note_geometry(&xs, &NoteAnchor::Left { at: 1 }, 3), (2, 7));
        assert_eq!(note_geometry(&xs, &NoteAnchor::Right { at: 0 }, 3), (4, 7));
    }

    #[test]
    fn row_height_covers_item_kinds() {
        assert_eq!(row_height(&SeqItem::Divider { text: "x".into() }), 2);
        assert_eq!(row_height(&SeqItem::Note { anchor: NoteAnchor::Left { at: 0 }, text: "x".into() }), 4);
        assert_eq!(row_height(&SeqItem::Message { from: 0, to: 0, text: None, dashed: false, head: "arrow" }), 4);
        assert_eq!(row_height(&SeqItem::Message { from: 0, to: 1, text: None, dashed: false, head: "arrow" }), 2);
    }

    #[test]
    fn layout_sequence_draws_participants_and_lifelines() {
        let canvas = layout_sequence(&sequence(vec![])).expect("layout");
        assert_eq!(canvas.w, 13);
        assert_eq!(canvas.h, 7);
    }

    #[test]
    fn layout_sequence_draws_messages_notes_and_dividers() {
        let canvas = layout_sequence(&sequence(vec![
            SeqItem::Message { from: 0, to: 1, text: Some("hello".into()), dashed: false, head: "arrow" },
            SeqItem::Note { anchor: NoteAnchor::Over { from: 0, to: 1 }, text: "note".into() },
            SeqItem::Divider { text: "loop".into() },
        ])).expect("layout");
        assert!(canvas.ch.iter().any(|c| c == "▶"));
        assert!(canvas.ch.iter().any(|c| c == "n"));
        assert!(canvas.ch.iter().any(|c| c == "─"));
    }
}
