//! Port of senpi `packages/tui/src/components/editor-line-render.ts`.
//!
//! Compose one visible editor row: mention ranges get the theme's mention style, the cursor
//! grapheme gets reverse video, and every fragment is styled on its own so the cursor's SGR
//! reset cannot bleed into the rest of a mention.

use crate::autocomplete::MentionRange;
use crate::utils::graphemes;

const FAKE_CURSOR_END: &str = "\x1b[7m \x1b[0m";

pub struct EditorLineCursor {
    pub pos: usize,
    pub marker: String,
    pub draw_fake_cursor: bool,
}

pub struct EditorLineRenderInput<'a> {
    pub text: &'a str,
    pub mentions: &'a [MentionRange],
    pub mention_style: &'a dyn Fn(&str) -> String,
    pub cursor: Option<EditorLineCursor>,
}

pub struct EditorLineRenderResult {
    pub text: String,
    /// True when a fake cursor cell was appended past the text (adds one column).
    pub cursor_appended: bool,
}

fn clamp_mentions(mentions: &[MentionRange], length: usize) -> Vec<MentionRange> {
    mentions
        .iter()
        .map(|range| MentionRange {
            start: range.start.min(length),
            end: range.end.min(length),
        })
        .filter(|range| range.start < range.end)
        .collect()
}

fn cut_points(
    length: usize,
    mentions: &[MentionRange],
    cursor_glyph: Option<MentionRange>,
) -> Vec<usize> {
    let mut cuts: Vec<usize> = vec![0, length];
    for range in mentions {
        cuts.push(range.start);
        cuts.push(range.end);
    }
    if let Some(glyph) = cursor_glyph {
        cuts.push(glyph.start);
        cuts.push(glyph.end);
    }
    cuts.sort_unstable();
    cuts.dedup();
    cuts
}

/// Compose one visible editor row; see the module docs for the styling contract.
pub fn render_editor_line(input: EditorLineRenderInput<'_>) -> EditorLineRenderResult {
    let text = input.text;
    let cursor = input.cursor.as_ref();
    let mentions = clamp_mentions(input.mentions, text.len());
    let cursor_in_text = cursor.is_some_and(|cursor| cursor.pos < text.len());
    let cursor_glyph = if cursor_in_text && cursor.is_some_and(|cursor| cursor.draw_fake_cursor) {
        let pos = cursor.expect("cursor present").pos;
        let first = graphemes(&text[pos..]).next().unwrap_or("");
        Some(MentionRange {
            start: pos,
            end: pos + first.len(),
        })
    } else {
        None
    };
    let points = cut_points(text.len(), &mentions, cursor_glyph);

    let mut out = String::new();
    for index in 0..points.len().saturating_sub(1) {
        let from = points[index];
        let to = points[index + 1];
        if let Some(cursor) = cursor {
            if cursor_in_text && from == cursor.pos {
                out.push_str(&cursor.marker);
            }
        }
        let segment = &text[from..to];
        let is_glyph = cursor_glyph.is_some_and(|glyph| from == glyph.start);
        let in_mention = mentions
            .iter()
            .any(|range| from >= range.start && to <= range.end);
        if is_glyph {
            out.push_str("\x1b[7m");
            out.push_str(segment);
            out.push_str("\x1b[0m");
        } else if in_mention {
            out.push_str(&(input.mention_style)(segment));
        } else {
            out.push_str(segment);
        }
    }

    if let Some(cursor) = cursor {
        if !cursor_in_text {
            out.push_str(&cursor.marker);
            if cursor.draw_fake_cursor {
                out.push_str(FAKE_CURSOR_END);
                return EditorLineRenderResult {
                    text: out,
                    cursor_appended: true,
                };
            }
        }
    }
    EditorLineRenderResult {
        text: out,
        cursor_appended: false,
    }
}
