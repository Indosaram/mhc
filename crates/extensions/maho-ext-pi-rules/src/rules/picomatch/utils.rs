//! Port of the pinned picomatch `lib/utils.js` (picomatch 4.0.5, MIT).
//!
//! The pinned module is a handful of small string helpers plus four regular
//! expressions. The regular expressions are hand-expanded here so the port
//! carries no extra dependency and so the JS semantics (UTF-16 unit indexing,
//! lazy `[...]` matching) are explicit.

/// `REGEX_SPECIAL_CHARS` character set — `[-*+?.^${}(|)[\]]`.
const SPECIAL_CHARS: &[char] = &['-', '*', '+', '?', '.', '^', '$', '{', '}', '(', '|', ')', '[', ']'];

/// `hasRegexChars` — `REGEX_SPECIAL_CHARS.test(str)`.
#[must_use]
pub fn has_regex_chars(value: &str) -> bool {
    value.chars().any(|ch| SPECIAL_CHARS.contains(&ch))
}

/// `isRegexChar` — a single regex-special character.
#[must_use]
pub fn is_regex_char(value: &str) -> bool {
    value.chars().count() == 1 && has_regex_chars(value)
}

/// `escapeRegex` — `REGEX_SPECIAL_CHARS_GLOBAL` replace with `\$1`.
#[must_use]
pub fn escape_regex(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if SPECIAL_CHARS.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// `toPosixSlashes` — `REGEX_BACKSLASH` (a backslash not followed by a regex
/// special character) replaced with `/`.
#[must_use]
pub fn to_posix_slashes(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(value.len());
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' {
            let excluded = chars.get(index + 1).is_some_and(|next| SPECIAL_CHARS.contains(next));
            if !excluded {
                out.push('/');
                index += 1;
                continue;
            }
        }
        out.push(ch);
        index += 1;
    }
    out
}

/// `isWindows` — process platform probe (`process.platform === 'win32'`).
#[must_use]
pub fn is_windows() -> bool {
    cfg!(windows)
}

/// `removeBackslashes` — `REGEX_REMOVE_BACKSLASH` replace.
///
/// Matches either a bracketed expression `[...]` (kept verbatim) or a lone
/// backslash that is followed by a character (dropped).
#[must_use]
pub fn remove_backslashes(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(value.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '['
            && let Some(end) = bracket_end(&chars, index)
        {
            out.extend(chars[index..=end].iter());
            index = end + 1;
            continue;
        }
        if chars[index] == '\\' && index + 1 < chars.len() {
            index += 1;
            continue;
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

/// Lazy `\[.*?[^\\]\]` end index starting at an opening bracket.
fn bracket_end(chars: &[char], start: usize) -> Option<usize> {
    let mut index = start + 2;
    while index < chars.len() {
        if chars[index] == ']' && chars[index - 1] != '\\' {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// `escapeLast` — escape the last unescaped occurrence of `ch` at or before `last_idx`.
#[must_use]
pub fn escape_last(input: &str, ch: char, last_idx: Option<usize>) -> String {
    let chars: Vec<char> = input.chars().collect();
    let from = match last_idx {
        Some(value) => value,
        None => chars.len().saturating_sub(1),
    };
    let mut found = None;
    let mut index = from as isize;
    while index >= 0 {
        if chars.get(index as usize) == Some(&ch) {
            found = Some(index as usize);
            break;
        }
        index -= 1;
    }
    let Some(idx) = found else {
        return input.to_string();
    };
    if idx > 0 && chars[idx - 1] == '\\' {
        return escape_last(input, ch, Some(idx - 1));
    }
    let mut out = String::with_capacity(input.len() + 1);
    out.extend(chars[..idx].iter());
    out.push('\\');
    out.extend(chars[idx..].iter());
    out
}

/// `removePrefix` — strip a leading `./`, reporting the prefix through `prefix`.
#[must_use]
pub fn remove_prefix(input: &str, prefix: &mut Option<String>) -> String {
    if let Some(rest) = input.strip_prefix("./") {
        *prefix = Some("./".into());
        rest.to_string()
    } else {
        input.to_string()
    }
}

/// `wrapOutput` — wrap a raw output string with anchors, honoring `negated`.
#[must_use]
pub fn wrap_output(input: &str, negated: bool, contains: bool) -> String {
    let (prepend, append) = if contains { ("", "") } else { ("^", "$") };
    let mut output = format!("{prepend}(?:{input}){append}");
    if negated {
        output = format!("(?:^(?!{output}).*$)");
    }
    output
}

/// `basename` — last path segment, or the previous one for a trailing separator.
#[must_use]
pub fn basename(path: &str, windows: bool) -> String {
    let segments: Vec<&str> = if windows {
        path.split(['\\', '/']).collect()
    } else {
        path.split('/').collect()
    };
    let last = segments.last().copied().unwrap_or("");
    if last.is_empty() {
        segments.get(segments.len().wrapping_sub(2)).copied().unwrap_or("").to_string()
    } else {
        last.to_string()
    }
}

/// Maps a Rust string into the JS UTF-16 code-unit domain.
///
/// Every UTF-16 code unit becomes one `char`: BMP units map to themselves and
/// lone-surrogate units map to a disjoint scalar range (`0x10000 + unit`) so
/// that supplementary input splits into the same two units JS sees. Both the
/// pattern (`parse::parse` and `parse::fastpaths`) and the matched input
/// (`Compiled::is_match`) go through this mapping, so the tokenizer, its `=== 1`
/// tests, its indexing, the generated regex source and the haystack all speak
/// UTF-16 units exactly like the pinned JS engine.
#[must_use]
pub fn js_units(value: &str) -> String {
    value
        .encode_utf16()
        .filter_map(|unit| {
            char::from_u32(if (0xd800..=0xdfff).contains(&unit) { 0x10000 + u32::from(unit) } else { u32::from(unit) })
        })
        .collect()
}

/// `str.length` — the pinned source measures strings in UTF-16 code units, so
/// every semantic `.length` / `=== 1` / `!== 1` test must use this rather than
/// `str.chars().count()`.
#[must_use]
pub fn js_length(value: &str) -> usize {
    value.encode_utf16().count()
}

/// True for the code points `js_units` produces for lone surrogate units
/// (`0x10000 + unit`, i.e. `U+1D800..=U+1DFFF`).
///
/// In a unit-mapped string such a `char` always stands for one UTF-16 code unit
/// and never for a scalar value of its own.
#[must_use]
pub fn is_unit_char(value: char) -> bool {
    (0x1_d800..=0x1_dfff).contains(&(value as u32))
}

/// `str.charCodeAt(index)` over the JS UTF-16 unit domain.
#[must_use]
pub fn code_at(chars: &[char], index: usize) -> Option<u32> {
    chars.get(index).map(|ch| *ch as u32)
}

/// The UTF-16 code unit a mapped character stands for: `js_units` maps a lone
/// surrogate unit `u` to `0x10000 + u`, and every other character is its own
/// unit.
#[must_use]
pub fn unit_value(value: char) -> u32 {
    let code = value as u32;
    if is_unit_char(value) { code - 0x10000 } else { code }
}

/// `Array#sort`'s default string comparison key: the UTF-16 code units of a
/// string in the mapped unit domain, compared lexicographically.
#[must_use]
pub fn js_unit_order(value: &str) -> Vec<u32> {
    value.chars().map(unit_value).collect()
}
