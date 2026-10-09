//! Port of the `grok-mermaid` `labels.ts` helpers used by `source-box.ts`.
use std::sync::LazyLock;

use regex::Regex;

use super::width::{measured, string_width};

/// Node labels wrap to at most this many display columns per line ...
pub const WRAP_WIDTH: usize = 24;
/// ... and at most this many lines; overflow is truncated with an ellipsis.
pub const MAX_LINES: usize = 4;
/// Edge labels are truncated to this many columns.
pub const MAX_LABEL: usize = 28;

/// Identifier-boundary characters preferred as break points when a single word
/// is too wide to fit, so it is not sliced mid-segment.
pub const LABEL_BREAK_CHARS: &[char] = &['_', '-', '.', '/'];

/// ASCII-only case folding, matching Rust's `to_ascii_lowercase`.
pub fn ascii_lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

pub fn ascii_upper(s: &str) -> String {
    s.to_ascii_uppercase()
}

static CONTROLS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("[\u{0}-\u{8}\u{b}\u{c}\u{e}-\u{1f}\u{7f}-\u{9f}]").expect("controls"));

pub fn strip_controls(src: &str) -> String {
    CONTROLS.replace_all(src, "").into_owned()
}

pub fn src_lines(src: &str) -> Vec<String> {
    let mut out: Vec<String> = src
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect();
    if out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

/// Matches Rust's `char::is_alphanumeric`.
pub fn is_alphanumeric(c: char) -> bool {
    c.is_alphanumeric()
}

/// Characters allowed in a bare node/state/class identifier.
pub fn is_id_char(c: char) -> bool {
    is_alphanumeric(c) || c == '_'
}

const ENTITY_LOOKAHEAD: usize = 10;

fn decode_entity_body(body: &str) -> Option<String> {
    match body {
        "lt" => Some("<".to_string()),
        "gt" => Some(">".to_string()),
        "amp" => Some("&".to_string()),
        "quot" => Some("\"".to_string()),
        "apos" => Some("'".to_string()),
        _ => {
            let num = body.strip_prefix('#')?;
            let (hex, digits) = if let Some(hex_digits) = num.strip_prefix(['x', 'X']) {
                (true, hex_digits)
            } else {
                (false, num)
            };
            if digits.is_empty() {
                return None;
            }
            let is_valid = if hex {
                digits.chars().all(|c| c.is_ascii_hexdigit())
            } else {
                digits.chars().all(|c| c.is_ascii_digit())
            };
            if !is_valid {
                return None;
            }
            let code = u32::from_str_radix(digits, if hex { 16 } else { 10 }).ok()?;
            // Surrogates and out-of-range values are not characters at all.
            if code > 0x10ffff || (0xd800..=0xdfff).contains(&code) {
                return None;
            }
            // Reject control chars: NUL collides with the CONT sentinel and ESC would
            // inject ANSI into scrollback.
            if code < 0x20 || (0x7f..=0x9f).contains(&code) {
                return None;
            }
            char::from_u32(code).map(|c| c.to_string())
        }
    }
}

/// Decode HTML entities in label text.
pub fn decode_html_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '&' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let hi = (i + 1 + ENTITY_LOOKAHEAD).min(chars.len());
        let mut semi = None;
        for (j, &c) in chars[..hi].iter().enumerate().skip(i + 1) {
            if c == ';' {
                semi = Some(j);
                break;
            }
        }
        let decoded = semi.and_then(|s_idx| {
            let body: String = chars[(i + 1)..s_idx].iter().collect();
            decode_entity_body(&body)
        });
        match decoded {
            None => {
                out.push('&');
                i += 1;
            }
            Some(repl) => {
                out.push_str(&repl);
                if let Some(s_idx) = semi {
                    i = s_idx + 1;
                } else {
                    i += 1;
                }
            }
        }
    }
    out
}

/// Strip markdown emphasis from a `` `backtick` `` label string.
pub fn strip_markdown(s: &str) -> String {
    let no_code: String = s.chars().filter(|&c| c != '`').collect();
    let no_strong = no_code.replace("**", "").replace("__", "");
    let chars: Vec<char> = no_strong.chars().collect();
    let mut out = String::new();
    for i in 0..chars.len() {
        let c = chars[i];
        let in_word = i > 0
            && is_alphanumeric(chars[i - 1])
            && i + 1 < chars.len()
            && is_alphanumeric(chars[i + 1]);
        if (c == '*' || c == '_') && !in_word {
            continue;
        }
        out.push(c);
    }
    out.trim().to_string()
}

const HTML_FORMAT_TAGS: &[&str] = &[
    "abbr", "b", "big", "cite", "code", "del", "em", "font", "i", "ins", "kbd", "mark",
    "pre", "q", "s", "samp", "small", "span", "strike", "strong", "sub", "sup", "tt",
    "u", "var",
];

fn html_tag_at(chars: &[char], start: usize) -> Option<(String, usize)> {
    if chars.get(start) != Some(&'<') {
        return None;
    }
    let mut i = start + 1;
    if i < chars.len() && chars[i] == '/' {
        i += 1;
    }
    let name_start = i;
    while i < chars.len() && chars[i].is_ascii_alphanumeric() {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    let name: String = chars[name_start..i].iter().collect();
    while i < chars.len() && chars[i] != '>' {
        if chars[i] == '<' {
            return None;
        }
        i += 1;
    }
    if i < chars.len() && chars[i] == '>' {
        Some((name, i + 1))
    } else {
        None
    }
}

pub fn strip_html_tags(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if let Some((name, end)) = html_tag_at(&chars, i) {
            let lower = name.to_ascii_lowercase();
            if lower == "br" {
                out.push(' ');
                i = end;
                continue;
            }
            if HTML_FORMAT_TAGS.contains(&lower.as_str()) {
                i = end;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn unwrap_delims<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    if s.len() >= open.len() + close.len() && s.starts_with(open) && s.ends_with(close) {
        Some(&s[open.len()..s.len() - close.len()])
    } else {
        None
    }
}

/// Normalise raw label text: strip markup, unquote, and decode entities.
pub fn clean_label(raw: &str) -> String {
    let stripped = strip_html_tags(raw.trim());
    let trimmed = stripped.trim();
    let unquoted = unwrap_delims(trimmed, "\"", "\"")
        .or_else(|| unwrap_delims(trimmed, "'", "'"))
        .unwrap_or(trimmed)
        .trim();
    let md = unwrap_delims(unquoted, "`", "`");
    let processed = match md {
        None => unquoted.to_string(),
        Some(inner) => strip_markdown(inner.trim()),
    };
    decode_html_entities(&processed)
}

fn last_break(s: &str) -> Option<usize> {
    s.rfind(|c| LABEL_BREAK_CHARS.contains(&c))
}

/// Wrap a label to `width` columns over at most `max_lines` lines.
pub fn wrap_label(label: &str, width: usize, max_lines: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0;

    for word in label.split_whitespace() {
        let ww = string_width(word);
        if ww > width {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            let mut chunk = String::new();
            let mut chunk_w = 0;
            for (ch, cw) in measured(word) {
                if chunk_w + cw > width && !chunk.is_empty() {
                    match last_break(&chunk) {
                        Some(p) => {
                            let prefix = &chunk[..=p];
                            let carry = &chunk[p + 1..];
                            lines.push(prefix.to_string());
                            chunk = carry.to_string();
                            chunk_w = string_width(&chunk);
                        }
                        None => {
                            lines.push(std::mem::take(&mut chunk));
                            chunk_w = 0;
                        }
                    }
                }
                chunk.push_str(ch);
                chunk_w += cw;
            }
            cur = chunk;
            cur_w = chunk_w;
        } else if cur.is_empty() {
            cur = word.to_string();
            cur_w = ww;
        } else if cur_w + 1 + ww <= width {
            cur.push(' ');
            cur.push_str(word);
            cur_w += 1 + ww;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
            cur_w = ww;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }

    if lines.len() > max_lines {
        lines.truncate(max_lines);
        let target = (width.saturating_sub(1)).max(1);
        let mut s = String::new();
        let mut sw = 0;
        if let Some(last) = lines.last() {
            for (ch, cw) in measured(last) {
                if sw + cw > target {
                    break;
                }
                s.push_str(ch);
                sw += cw;
            }
        }
        if let Some(last) = lines.last_mut() {
            *last = format!("{s}…");
        }
    }
    lines
}

/// Truncate to `inner` columns, leaving room for the ellipsis.
pub fn fit_label(label: &str, inner: usize) -> String {
    if string_width(label) <= inner {
        return label.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for (c, cw) in measured(label) {
        if used + cw + 1 > inner {
            break;
        }
        out.push_str(c);
        used += cw;
    }
    format!("{out}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(WRAP_WIDTH, 24);
        assert_eq!(MAX_LINES, 4);
        assert_eq!(MAX_LABEL, 28);
        assert_eq!(LABEL_BREAK_CHARS, &['_', '-', '.', '/']);
    }

    #[test]
    fn test_ascii_lower() {
        assert_eq!(ascii_lower("Hello World 123! \u{0130}"), "hello world 123! \u{0130}");
    }

    #[test]
    fn test_ascii_upper() {
        assert_eq!(ascii_upper("Hello World 123! \u{0131}"), "HELLO WORLD 123! \u{0131}");
    }

    #[test]
    fn test_strip_controls() {
        assert_eq!(strip_controls("foo\x00bar\x1fbaz\t\n\r"), "foobarbaz\t\n\r");
    }

    #[test]
    fn test_src_lines() {
        assert_eq!(src_lines("a\r\nb\nc\n"), vec!["a", "b", "c"]);
    }

    #[test]
    fn test_is_alphanumeric() {
        assert!(is_alphanumeric('a'));
        assert!(is_alphanumeric('1'));
        assert!(!is_alphanumeric('_'));
        assert!(!is_alphanumeric(' '));
    }

    #[test]
    fn test_is_id_char() {
        assert!(is_id_char('a'));
        assert!(is_id_char('_'));
        assert!(!is_id_char('-'));
    }

    #[test]
    fn test_decode_html_entities() {
        assert_eq!(decode_html_entities("&lt;hello &amp; world&gt;"), "<hello & world>");
        assert_eq!(decode_html_entities("&quot;foo&apos;"), "\"foo'");
        assert_eq!(decode_html_entities("&#65; &#x42;"), "A B");
        assert_eq!(decode_html_entities("&amp;lt;"), "&lt;");
        assert_eq!(decode_html_entities("&#0;"), "&#0;");
        assert_eq!(decode_html_entities("&unknown;"), "&unknown;");
    }

    #[test]
    fn test_strip_markdown() {
        assert_eq!(
            strip_markdown("`code` **bold** __strong__ *italic* _em_"),
            "code bold strong italic em"
        );
        assert_eq!(strip_markdown("foo_bar and a*b"), "foo_bar and a*b");
    }

    #[test]
    fn test_strip_html_tags() {
        assert_eq!(
            strip_html_tags("<b>bold</b> <br> line2 <br/> line3 <i>italic</i>"),
            "bold   line2   line3 italic"
        );
        assert_eq!(strip_html_tags("Vec<String> and <id>"), "Vec<String> and <id>");
    }

    #[test]
    fn test_clean_label() {
        assert_eq!(clean_label("  \"<b>&lt;hi&gt;</b>\"  "), "<hi>");
        assert_eq!(clean_label("`**bold**`"), "bold");
    }

    #[test]
    fn test_wrap_label() {
        assert_eq!(wrap_label("short label", 24, 4), vec!["short label"]);
        assert_eq!(
            wrap_label(
                "a very long label that exceeds the wrap width and wraps onto multiple lines",
                20,
                2
            ),
            vec!["a very long label", "that exceeds the…"]
        );
        assert_eq!(
            wrap_label("single_very_long_word_with_break_chars", 10, 4),
            vec!["single_", "very_long_", "word_with_", "break_…"]
        );
        assert_eq!(
            wrap_label("singleverylongwordwithoutbreakchars", 10, 4),
            vec!["singlevery", "longwordwi", "thoutbreak", "chars"]
        );
    }

    #[test]
    fn test_fit_label() {
        assert_eq!(fit_label("short", 10), "short");
        assert_eq!(fit_label("very long label", 10), "very long…");
    }
}
