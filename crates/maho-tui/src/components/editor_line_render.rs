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

/// senpi `KeywordRange`: the painted slice of one row plus the whole run it belongs to, so the
/// per-character spectrum stays continuous across wrapped rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeywordRange {
    pub start: usize,
    pub end: usize,
    pub run_start: isize,
    pub run_length: usize,
}

pub type KeywordStyle<'a> = &'a dyn Fn(&str, usize, usize) -> String;

pub struct EditorLineRenderInput<'a> {
    pub keywords: &'a [KeywordRange],
    pub keyword_style: Option<KeywordStyle<'a>>,
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

/// Snap one byte offset down to a character boundary. Cut points come from clamped ranges, so this
/// only matters for a caller that built byte offsets into the middle of a character, but a row must
/// never panic or duplicate a character over it.
fn floor_to_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Compose one visible editor row; see the module docs for the styling contract.
pub fn render_editor_line(input: EditorLineRenderInput<'_>) -> EditorLineRenderResult {
    let text = input.text;
    let cursor = input.cursor.as_ref();
    let mentions = clamp_mentions(input.mentions, text.len());
    let keywords: Vec<KeywordRange> = match input.keyword_style {
        Some(_) => input
            .keywords
            .iter()
            .copied()
            .filter(|range| range.start < text.len() && range.start < range.end)
            .collect(),
        None => Vec::new(),
    };
    let cursor_in_text = cursor.is_some_and(|cursor| cursor.pos < text.len());
    let cursor_glyph = if cursor_in_text && cursor.is_some_and(|cursor| cursor.draw_fake_cursor) {
        let pos = cursor.expect("cursor present").pos;
        // A cursor parked inside a multi-byte character would panic the slice below; treat it as
        // plain text instead of taking the whole renderer down.
        text.get(pos..)
            .and_then(|rest| graphemes(rest).next())
            .map(|first| MentionRange {
                start: pos,
                end: pos + first.len(),
            })
    } else {
        None
    };
    let mut cut_ranges = mentions.clone();
    cut_ranges.extend(clamp_mentions(
        &keywords
            .iter()
            .map(|range| MentionRange {
                start: range.start,
                end: range.end,
            })
            .collect::<Vec<_>>(),
        text.len(),
    ));
    let points = cut_points(text.len(), &cut_ranges, cursor_glyph);
    // Snap once, up front: snapping each segment independently would let two neighbours both cover
    // the character a cut point landed inside, printing it twice.
    let points: Vec<usize> = {
        let mut snapped: Vec<usize> = points
            .into_iter()
            .map(|point| floor_to_boundary(text, point))
            .collect();
        snapped.dedup();
        snapped
    };

    let mut out = String::new();
    for index in 0..points.len().saturating_sub(1) {
        let from = points[index];
        let to = points[index + 1];
        if let Some(cursor) = cursor
            && cursor_in_text
            && from == cursor.pos
        {
            out.push_str(&cursor.marker);
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
        } else if let (Some(style), Some(range)) = (
            input.keyword_style,
            keywords
                .iter()
                .find(|range| from >= range.start && to <= range.end),
        ) {
            let mut offset = from;
            for ch in segment.chars() {
                let mut buf = [0u8; 4];
                out.push_str(&style(
                    ch.encode_utf8(&mut buf),
                    usize::try_from(offset as isize - range.run_start).unwrap_or(0),
                    range.run_length,
                ));
                offset += ch.len_utf8();
            }
        } else {
            out.push_str(segment);
        }
    }

    if let Some(cursor) = cursor
        && !cursor_in_text
    {
        out.push_str(&cursor.marker);
        if cursor.draw_fake_cursor {
            out.push_str(FAKE_CURSOR_END);
            return EditorLineRenderResult {
                text: out,
                cursor_appended: true,
            };
        }
    }
    EditorLineRenderResult {
        text: out,
        cursor_appended: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cut points come from clamped ranges, but a caller that builds byte offsets itself must not
    /// be able to panic the renderer by pointing into the middle of a character.
    #[test]
    fn survives_cut_points_inside_multibyte_characters() {
        let text = "\u{d55c}\u{ae00} \u{1f600} ulw";
        let cursor = EditorLineCursor {
            pos: 1,
            marker: String::new(),
            draw_fake_cursor: true,
        };
        let rendered = render_editor_line(EditorLineRenderInput {
            text,
            cursor: Some(cursor),
            mentions: &[MentionRange { start: 1, end: 4 }],
            mention_style: &|segment| segment.to_owned(),
            keyword_style: None,
            keywords: &[],
        });
        // Exactly the input once: no panic, and no character printed twice by two neighbouring
        // segments both covering the character a bad cut point landed inside.
        assert_eq!(rendered.text, text);
    }
}
