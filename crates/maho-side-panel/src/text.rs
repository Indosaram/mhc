//! Cell widths, sanitizing and fitting rows to the panel width (spec 4).

/// The truncation marker.
pub const ELLIPSIS: char = '…';

fn is_zero_width(c: char) -> bool {
    let u = c as u32;
    matches!(u,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x0610..=0x061A | 0x064B..=0x065F
        | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E
        | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F
        | 0x200B..=0x200D | 0x2060 | 0xFEFF
        | 0xFE00..=0xFE0F | 0xE0100..=0xE01EF
        | 0x1F3FB..=0x1F3FF)
}

fn is_wide(c: char) -> bool {
    let u = c as u32;
    matches!(u,
        0x1100..=0x115F | 0x231A..=0x231B | 0x2329..=0x232A | 0x23E9..=0x23EC | 0x23F0 | 0x23F3
        | 0x25FD..=0x25FE | 0x2614..=0x2615 | 0x2648..=0x2653 | 0x267F | 0x2693 | 0x26A1
        | 0x26AA..=0x26AB | 0x26BD..=0x26BE | 0x26C4..=0x26C5 | 0x26CE | 0x26D4 | 0x26EA
        | 0x26F2..=0x26F3 | 0x26F5 | 0x26FA | 0x26FD | 0x2705 | 0x270A..=0x270B | 0x2728 | 0x274C
        | 0x274E | 0x2753..=0x2755 | 0x2757 | 0x2795..=0x2797 | 0x27B0 | 0x27BF | 0x2B1B..=0x2B1C
        | 0x2B50 | 0x2B55
        | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF | 0xA960..=0xA97F | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF
        | 0xFE10..=0xFE19 | 0xFE30..=0xFE6F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6
        | 0x1F004 | 0x1F0CF | 0x1F18E | 0x1F191..=0x1F19A | 0x1F200..=0x1F251
        | 0x1F300..=0x1F64F | 0x1F680..=0x1F6FF | 0x1F7E0..=0x1F7EB | 0x1F90C..=0x1F9FF
        | 0x1FA70..=0x1FAFF | 0x20000..=0x2FFFD | 0x30000..=0x3FFFD)
}

/// Terminal cells of one character (ESC sequences are handled by [`visible_width`]).
pub fn char_width(c: char) -> usize {
    if is_zero_width(c) || c.is_control() {
        0
    } else if is_wide(c) {
        2
    } else {
        1
    }
}

/// Length in bytes of an SGR sequence (`ESC [ ... m`) starting at `s`, if `s` starts with one.
fn sgr_len(s: &str) -> Option<usize> {
    let rest = s.strip_prefix("\x1b[")?;
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == ';' || c == ':'))?;
    (rest.as_bytes()[end] == b'm').then_some(2 + end + 1)
}

/// Length in bytes of an OSC sequence (`ESC ] ... BEL` or `ESC ] ... ESC \`) starting at `s`.
fn osc_len(s: &str) -> Option<usize> {
    let rest = s.strip_prefix("\x1b]")?;
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x07 {
            return Some(2 + i + 1);
        }
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
            return Some(2 + i + 2);
        }
        i += 1;
    }
    None
}

/// Terminal cells of a rendered row: SGR and OSC sequences count 0.
pub fn visible_width(s: &str) -> usize {
    let mut width = 0;
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if let Some(n) = sgr_len(rest).or_else(|| osc_len(rest)) {
            i += n;
            continue;
        }
        let c = rest.chars().next().unwrap_or(' ');
        width += char_width(c);
        i += c.len_utf8();
    }
    width
}

fn is_bidi_control(c: char) -> bool {
    matches!(c as u32, 0x061C | 0x200E | 0x200F | 0x202A..=0x202E | 0x2066..=0x2069)
}

/// Removes bidi controls, turns tab/newline/CR/VT/FF/NEL into a space and drops every other
/// control character (ESC and BEL included). Applied to all data text before rendering.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\t' | '\n' | '\r' | '\x0b' | '\x0c' | '\u{85}' => Some(' '),
            c if is_bidi_control(c) => None,
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

/// Pads or truncates a row to exactly `width` cells. Styling sequences are kept; truncation
/// ends with `…` except at width 1. A styled row that is cut gets a reset appended.
pub fn fit(row: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let total = visible_width(row);
    if total <= width {
        let mut out = row.to_owned();
        out.push_str(&" ".repeat(width - total));
        return out;
    }
    let budget = if width == 1 { 1 } else { width - 1 };
    let mut out = String::new();
    let mut used = 0;
    let mut styled = false;
    let mut cut = false;
    let mut i = 0;
    while i < row.len() {
        let rest = &row[i..];
        // Escape sequences are always kept so links and colors stay closed after a cut.
        if let Some(n) = sgr_len(rest).or_else(|| osc_len(rest)) {
            out.push_str(&rest[..n]);
            styled = true;
            i += n;
            continue;
        }
        let c = rest.chars().next().unwrap_or(' ');
        i += c.len_utf8();
        if cut {
            continue;
        }
        let w = char_width(c);
        if used + w > budget {
            cut = true;
            if width > 1 {
                out.push(ELLIPSIS);
            }
            out.push_str(&" ".repeat(width.saturating_sub(used + usize::from(width > 1))));
            continue;
        }
        out.push(c);
        used += w;
    }
    if styled {
        out.push_str("\x1b[0m");
    }
    out
}

/// Fits every row; width 0 yields no rows.
pub fn fit_rows(rows: &[String], width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    rows.iter().map(|row| fit(row, width)).collect()
}

/// Keeps the end of a path visible: `…tail` within `max` cells.
pub fn path_tail(path: &str, max: usize) -> String {
    if visible_width(path) <= max {
        return path.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut tail: Vec<char> = Vec::new();
    let mut used = 1;
    for c in path.chars().rev() {
        let w = char_width(c);
        if used + w > max {
            break;
        }
        used += w;
        tail.push(c);
    }
    let mut out = String::from(ELLIPSIS);
    out.extend(tail.into_iter().rev());
    out
}

/// Wraps plain text at `width` cells on spaces (long words are split).
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut used = 0;
        for word in paragraph.split(' ').filter(|w| !w.is_empty()) {
            let ww = visible_width(word);
            if used > 0 && used + 1 + ww > width {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            if ww > width {
                for c in word.chars() {
                    let cw = char_width(c);
                    if used + cw > width {
                        lines.push(std::mem::take(&mut line));
                        used = 0;
                    }
                    line.push(c);
                    used += cw;
                }
                continue;
            }
            if used > 0 {
                line.push(' ');
                used += 1;
            }
            line.push_str(word);
            used += ww;
        }
        lines.push(line);
    }
    lines
}

/// Compact token count: `950`, `12.3k`, `1.2M`.
pub fn compact_count(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Short duration: `45s`, `3m12s`, `1h04m`.
pub fn duration(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

/// A `width`-cell bar filled to `percent` (0..=100).
pub fn bar(percent: f64, width: usize) -> String {
    let pct = percent.clamp(0.0, 100.0);
    let filled = ((pct / 100.0) * width as f64).round() as usize;
    format!(
        "{}{}",
        "█".repeat(filled),
        "░".repeat(width - filled.min(width))
    )
}
