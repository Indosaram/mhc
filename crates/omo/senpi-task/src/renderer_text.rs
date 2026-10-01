//! Terminal-safe text shaping shared by status rows and renderers (port of `renderer-text.ts`).

use unicode_width::UnicodeWidthChar;

const DEFAULT_EXCERPT_WIDTH: usize = 120;
pub const ELLIPSIS: &str = "...";

/// pi-tui `visibleWidth`: ANSI escape sequences occupy no cells and a tab counts as 3.
pub fn renderer_visible_width(value: &str) -> usize {
    let chars: Vec<char> = value.chars().collect();
    let mut width = 0;
    let mut index = 0;
    while index < chars.len() {
        if let Some(length) = crate::tools::render::ansi_code_length(&chars, index) {
            index += length;
            continue;
        }
        width += if chars[index] == '\t' { 3 } else { char_width(chars[index]) };
        index += 1;
    }
    width
}

pub fn normalize_renderer_text(value: &str) -> String {
    collapse_whitespace(&strip_terminal_controls(value))
}

pub fn excerpt_renderer_text(value: &str, width: Option<usize>) -> String {
    let width = width.unwrap_or(DEFAULT_EXCERPT_WIDTH);
    let normalized = normalize_renderer_text(value);
    if width == 0 {
        return String::new();
    }
    strip_terminal_controls(&truncate_to_width(&normalized, width, ELLIPSIS))
}

pub fn excerpt_renderer_prompt_text(value: &str, width: Option<usize>) -> String {
    let width = width.unwrap_or(DEFAULT_EXCERPT_WIDTH);
    let normalized = normalize_renderer_text(value);
    if width == 0 {
        return String::new();
    }
    if renderer_visible_width(&normalized) <= width {
        return normalized;
    }
    let content_width = width.saturating_sub(renderer_visible_width(ELLIPSIS));
    let clipped = truncate_to_width(&normalized, content_width, "");
    match last_word_boundary(&clipped) {
        Some(boundary) if boundary > 0 => format!("{}{ELLIPSIS}", clipped[..boundary].trim_end()),
        _ => strip_terminal_controls(&truncate_to_width(&normalized, width, ELLIPSIS)),
    }
}

pub fn join_renderer_tokens(tokens: &[Option<&str>]) -> String {
    tokens
        .iter()
        .flatten()
        .filter(|token| !token.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn optional_renderer_text(value: Option<&str>) -> Option<String> {
    let normalized = normalize_renderer_text(value?);
    (!normalized.is_empty()).then_some(normalized)
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Byte offset of the start of the trailing `\s+\S*$` run, mirroring `String.prototype.search`.
fn last_word_boundary(value: &str) -> Option<usize> {
    let trailing_word_start = value
        .char_indices()
        .rev()
        .take_while(|(_, ch)| !ch.is_whitespace())
        .last()
        .map_or(value.len(), |(index, _)| index);
    let head = &value[..trailing_word_start];
    if !head.ends_with(char::is_whitespace) {
        return None;
    }
    Some(head.trim_end_matches(char::is_whitespace).len())
}

/// pi-tui `truncateToWidth` for already-stripped text: keeps the widest contiguous char prefix
/// that leaves room for the ellipsis. The trailing SGR resets pi-tui appends are omitted because
/// every caller strips terminal controls from the result.
fn truncate_to_width(text: &str, max_width: usize, ellipsis: &str) -> String {
    if max_width == 0 || text.is_empty() {
        return String::new();
    }
    let ellipsis_width = renderer_visible_width(ellipsis);
    if ellipsis_width >= max_width {
        if renderer_visible_width(text) <= max_width {
            return text.to_string();
        }
        return clip_to_width(ellipsis, max_width);
    }
    if renderer_visible_width(text) <= max_width {
        return text.to_string();
    }
    format!(
        "{}{ellipsis}",
        clip_to_width(text, max_width - ellipsis_width)
    )
}

fn clip_to_width(text: &str, target_width: usize) -> String {
    let mut kept = String::new();
    let mut kept_width = 0;
    for ch in text.chars() {
        let width = char_width(ch);
        if kept_width + width > target_width {
            break;
        }
        kept.push(ch);
        kept_width += width;
    }
    kept
}

fn strip_terminal_controls(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut text = String::new();
    let mut index = 0;
    while index < chars.len() {
        let code = chars[index] as u32;
        if code == 0x1b {
            index = skip_escape_sequence(&chars, index);
            continue;
        }
        if code == 0x9b {
            index = skip_csi(&chars, index + 1);
            continue;
        }
        if matches!(code, 0x90 | 0x98 | 0x9d | 0x9e | 0x9f) {
            index = skip_control_string(&chars, index + 1, code == 0x9d);
            continue;
        }
        if code <= 0x1f || (0x7f..=0x9f).contains(&code) {
            if (0x09..=0x0d).contains(&code) {
                text.push(' ');
            }
            index += 1;
            continue;
        }
        text.push(chars[index]);
        index += 1;
    }
    text
}

fn skip_escape_sequence(chars: &[char], escape_index: usize) -> usize {
    let next_index = escape_index + 1;
    let Some(&next) = chars.get(next_index) else {
        return chars.len();
    };
    let next = next as u32;
    if next == 0x5b {
        return skip_csi(chars, next_index + 1);
    }
    if matches!(next, 0x50 | 0x58 | 0x5d | 0x5e | 0x5f) {
        return skip_control_string(chars, next_index + 1, next == 0x5d);
    }
    let mut index = next_index;
    while index < chars.len() && (0x20..=0x2f).contains(&(chars[index] as u32)) {
        index += 1;
    }
    if index < chars.len() && (0x30..=0x7e).contains(&(chars[index] as u32)) {
        return index + 1;
    }
    next_index
}

fn skip_csi(chars: &[char], start_index: usize) -> usize {
    (start_index..chars.len())
        .find(|&index| (0x40..=0x7e).contains(&(chars[index] as u32)))
        .map_or(chars.len(), |index| index + 1)
}

fn skip_control_string(chars: &[char], start_index: usize, bell_terminates: bool) -> usize {
    for index in start_index..chars.len() {
        let code = chars[index] as u32;
        if bell_terminates && code == 0x07 {
            return index + 1;
        }
        if code == 0x9c {
            return index + 1;
        }
        if code == 0x1b && chars.get(index + 1) == Some(&'\\') {
            return index + 2;
        }
    }
    chars.len()
}
