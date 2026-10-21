use unicode_general_category::{get_general_category, GeneralCategory};
pub fn sanitize_terminal_text(text: &str) -> String {
    text.chars().filter_map(|c| {
        if matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') { None }
        else if c.is_control() { matches!(c, '\t' | '\n' | '\x0b' | '\x0c' | '\r' | '\u{0085}').then_some(' ') }
        else { Some(c) }
    }).collect()
}
fn character_width(c: char) -> usize {
    let code = u32::from(c);
    if c.is_control() || matches!(c, '\u{200d}' | '\u{fe0e}' | '\u{fe0f}') || matches!(get_general_category(c), GeneralCategory::NonspacingMark | GeneralCategory::SpacingMark | GeneralCategory::EnclosingMark) { return 0; }
    usize::from(code >= 0x1100 && (code <= 0x115f || code == 0x2329 || code == 0x232a || ((0x2e80..=0xa4cf).contains(&code) && code != 0x303f) || (0xac00..=0xd7a3).contains(&code) || (0xf900..=0xfaff).contains(&code) || (0xfe10..=0xfe19).contains(&code) || (0xfe30..=0xfe6f).contains(&code) || (0xff00..=0xff60).contains(&code) || (0xffe0..=0xffe6).contains(&code) || (0x1f300..=0x1faff).contains(&code) || (0x20000..=0x3fffd).contains(&code))) + 1
}
fn sgr_len(text: &str) -> Option<usize> {
    let rest = text.strip_prefix("\x1b[")?;
    let n = rest.bytes().take_while(|b| b.is_ascii_digit() || *b == b';').count();
    (rest.as_bytes().get(n) == Some(&b'm')).then_some(n + 3)
}
pub fn visible_width(text: &str) -> usize {
    let mut rest = text; let mut width = 0;
    while !rest.is_empty() { if let Some(n) = sgr_len(rest) { rest = &rest[n..]; } else if let Some(c) = rest.chars().next() { width += character_width(c); rest = &rest[c.len_utf8()..]; } }
    width
}
pub fn pad_visible(text: &str, width: usize) -> String { format!("{text}{}", " ".repeat(width.saturating_sub(visible_width(text)))) }
pub fn truncate_visible(text: &str, width: usize) -> String {
    if width == 0 { return String::new(); }
    if visible_width(text) <= width { return text.into(); }
    let budget = if width > 1 { width - 1 } else { width };
    let mut out = String::new(); let mut rest = text; let mut used = 0; let mut colored = false;
    while !rest.is_empty() {
        if let Some(n) = sgr_len(rest) { let sgr = &rest[..n]; colored = sgr != "\x1b[39m" && sgr != "\x1b[0m"; out.push_str(sgr); rest = &rest[n..]; continue; }
        if let Some(c) = rest.chars().next() { let cells = character_width(c); if used + cells > budget { break; } out.push(c); used += cells; rest = &rest[c.len_utf8()..]; }
    }
    if width > 1 { out.push('…'); }
    if colored { out.push_str("\x1b[39m"); }
    out
}
pub fn truncate_visible_start(text: &str, width: usize) -> String {
    if width == 0 { return String::new(); }
    if visible_width(text) <= width { return text.into(); }
    let mut plain = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(length) = sgr_len(rest) { rest = &rest[length..]; }
        else if let Some(character) = rest.chars().next() { plain.push(character); rest = &rest[character.len_utf8()..]; }
    }
    let budget = width.saturating_sub(usize::from(width > 1)); let mut used = 0;
    let tail: String = plain.chars().rev().take_while(|c| { used += character_width(*c); used <= budget }).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{}{tail}", if width > 1 { "…" } else { "" })
}

pub fn wrap_visible(text: &str, width: usize) -> Vec<String> {
    if width == 0 { return Vec::new(); }
    let mut rows = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() { rows.push(String::new()); continue; }
        let mut row = String::new(); let mut token = String::new(); let mut space = None;
        let mut tokens = Vec::new();
        for c in paragraph.chars() { if space.is_some_and(|s| s != c.is_whitespace()) { tokens.push(std::mem::take(&mut token)); } space = Some(c.is_whitespace()); token.push(c); } tokens.push(token);
        for token in tokens { let mut pending = token.as_str(); while !pending.is_empty() {
            if visible_width(pending) <= width.saturating_sub(visible_width(&row)) { row.push_str(pending); break; }
            if !row.is_empty() {
                if pending == " " { rows.push(std::mem::take(&mut row)); break; }
                if !pending.starts_with(char::is_whitespace) && row.ends_with(' ') && !row.ends_with("  ") { row.pop(); }
                rows.push(std::mem::take(&mut row));
                continue;
            }
            let mut used = 0; let mut end = 0; for (i,c) in pending.char_indices() { let cells = character_width(c); if used + cells > width { break; } used += cells; end = i + c.len_utf8(); }
            if end == 0 { end = pending.chars().next().map_or(0, char::len_utf8); }
            rows.push(pending[..end].into()); pending = &pending[end..];
        } }
        if !row.is_empty() { rows.push(row); }
    } rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_truncation_removes_sgr_without_counting_it() {
        let text = "\x1b[31mfolder/파일.rs\x1b[0m";
        assert_eq!(truncate_visible_start(text, 6), "…일.rs");
        assert_eq!(visible_width(&truncate_visible_start(text, 6)), 6);
        assert_eq!(truncate_visible_start(text, 0), "");
        assert_eq!(truncate_visible_start("\u{0301}", 0), "");
    }
}
