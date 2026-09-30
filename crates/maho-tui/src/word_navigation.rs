//! Word-wise cursor movement (port of senpi `word-navigation.ts`). Cursors are byte offsets.

use crate::utils::{WordSegment, is_punctuation_char, is_whitespace_char, word_segments};

type SegmentFn<'f> = dyn Fn(&str) -> Vec<WordSegment<'_>> + 'f;

/// Options for word navigation. Defaults to Unicode word segmentation.
#[derive(Default)]
pub struct WordNavigationOptions<'f> {
    /// Custom segmenter returning word segments for the given text.
    pub segment: Option<&'f SegmentFn<'f>>,
    /// Identifies atomic segments treated as single units (e.g. paste markers).
    pub is_atomic_segment: Option<&'f dyn Fn(&str) -> bool>,
}

fn segments_of<'t>(text: &'t str, options: &WordNavigationOptions<'_>) -> Vec<WordSegment<'t>> {
    match options.segment {
        Some(f) => f(text),
        None => word_segments(text),
    }
}

fn atomic(options: &WordNavigationOptions<'_>, segment: &str) -> bool {
    options.is_atomic_segment.is_some_and(|f| f(segment))
}

/// Cursor after moving one word backward: skip trailing whitespace, then stop at the next
/// word/punctuation boundary.
pub fn find_word_backward(text: &str, cursor: usize, options: &WordNavigationOptions<'_>) -> usize {
    if cursor == 0 {
        return 0;
    }
    let mut segments = segments_of(&text[..cursor], options);
    let mut new_cursor = cursor;

    while let Some(last) = segments.last() {
        if atomic(options, last.segment) || !is_whitespace_char(last.segment) {
            break;
        }
        new_cursor -= last.segment.len();
        segments.pop();
    }

    let Some(last) = segments.last().copied() else {
        return new_cursor;
    };

    if atomic(options, last.segment) {
        new_cursor -= last.segment.len();
    } else if last.is_word_like {
        // Stop just after the last ASCII punctuation inside the word-like segment.
        match last.segment.rfind(is_punctuation_char) {
            None => new_cursor -= last.segment.len(),
            Some(idx) => {
                let punct_len = last.segment[idx..].chars().next().map_or(1, char::len_utf8);
                new_cursor -= last.segment.len() - (idx + punct_len);
            }
        }
    } else {
        while let Some(last) = segments.last() {
            if atomic(options, last.segment)
                || last.is_word_like
                || is_whitespace_char(last.segment)
            {
                break;
            }
            new_cursor -= last.segment.len();
            segments.pop();
        }
    }
    new_cursor
}

/// Cursor after moving one word forward: skip leading whitespace, then stop at the next
/// word/punctuation boundary.
pub fn find_word_forward(text: &str, cursor: usize, options: &WordNavigationOptions<'_>) -> usize {
    if cursor >= text.len() {
        return text.len();
    }
    let segments = segments_of(&text[cursor..], options);
    let mut iter = segments.into_iter().peekable();
    let mut new_cursor = cursor;

    while let Some(next) = iter.peek() {
        if atomic(options, next.segment) || !is_whitespace_char(next.segment) {
            break;
        }
        new_cursor += next.segment.len();
        iter.next();
    }

    let Some(next) = iter.next() else {
        return new_cursor;
    };

    if atomic(options, next.segment) {
        new_cursor += next.segment.len();
    } else if next.is_word_like {
        // Stop at the first ASCII punctuation inside the word-like segment.
        new_cursor += next
            .segment
            .find(is_punctuation_char)
            .unwrap_or(next.segment.len());
    } else {
        new_cursor += next.segment.len();
        while let Some(seg) = iter.peek() {
            if atomic(options, seg.segment) || seg.is_word_like || is_whitespace_char(seg.segment) {
                break;
            }
            new_cursor += seg.segment.len();
            iter.next();
        }
    }
    new_cursor
}

#[cfg(test)]
#[path = "word_navigation_tests.rs"]
mod tests;
