//! Credential redaction for memory-repository URLs.

use std::sync::LazyLock;

const MASK: &str = "***";

/// Upstream `SECRET_PATTERN_SOURCES`, in replacement order: each pattern source
/// paired with the JavaScript flags it is compiled with.
///
/// The patterns are matched through the port's established `regress` UTF-16 seam
/// (`Regex::from_unicode` over UTF-16 code units, then `find_from_ucs2`), which
/// reproduces the non-`u` JavaScript semantics: `\b` is ASCII-only, `\s`
/// includes U+FEFF and excludes U+0085, and `\S{1,256}` counts UTF-16 code units.
const SECRET_PATTERN_SOURCES: [(&str, &str); 5] = [
    (r"\bAKIA[0-9A-Z]{16}\b", ""),
    (
        r"\b(?:bearer|token|api[_-]?key|secret|password|passwd|pwd)\s*[=:]\s*\S{1,256}",
        "i",
    ),
    (r"\bAuthorization\s*:\s*Bearer\s+\S{1,256}", "i"),
    (r"\bsk-(?:proj-)?[-_A-Za-z0-9]+\b", ""),
    (r"\b(?:ghp|github_pat|glpat|xox[baprs])-[-_A-Za-z0-9]+\b", ""),
];
const SPLIT_CREDENTIAL_ASSIGNMENT: &str = r#"(?:a\s*u\s*t\s*h\s*o\s*r\s*i\s*z\s*a\s*t\s*i\s*o\s*n|p\s*a\s*s\s*s\s*w\s*(?:o\s*r\s*)?d|s\s*e\s*c\s*r\s*e\s*t|t\s*o\s*k\s*e\s*n|a\s*p\s*i\s*[_-]?\s*k\s*e\s*y)\s*[:=]\s*\S{6,256}"#;

/// Compiled once. Upstream keeps a non-global and a global copy of every pattern
/// only because a JavaScript `RegExp` carries `lastIndex` state; `regress`
/// matches are stateless, so one set serves both the predicate and the replacement.
static SECRET_PATTERNS: LazyLock<Vec<regress::Regex>> = LazyLock::new(|| {
    SECRET_PATTERN_SOURCES
        .iter()
        .map(|(source, flags)| {
            regress::Regex::from_unicode(source.encode_utf16().map(u32::from), *flags)
                .expect("static regex literals are valid")
        })
        .collect()
});
static SPLIT_CREDENTIAL_PATTERN: LazyLock<regress::Regex> = LazyLock::new(|| {
    regress::Regex::from_unicode(
        SPLIT_CREDENTIAL_ASSIGNMENT.encode_utf16().map(u32::from),
        "i",
    )
    .expect("static split credential regex is valid")
});

/// True when `value` holds secret-like material (upstream `containsSecretLikeMaterial`).
pub fn contains_secret_like_material(value: &str) -> bool {
    if normalized_secret_detect(value) {
        return true;
    }
    let units: Vec<u16> = value.encode_utf16().collect();
    SECRET_PATTERNS
        .iter()
        .any(|pattern| pattern.find_from_ucs2(&units, 0).next().is_some())
        || find_pem_block(&units, 0).is_some()
}

/// Mask every secret-like pattern in upstream order, then every complete PEM block.
pub fn redact_secret_like_material(value: &str) -> String {
    let normalized = normalized_secret_spans(value);
    if !normalized.is_empty() {
        return mask_spans(value, normalized);
    }
    let mut units: Vec<u16> = value.encode_utf16().collect();
    for pattern in SECRET_PATTERNS.iter() {
        units = replace_all_ucs2(pattern, &units);
    }
    let mask_len = MASK.encode_utf16().count();
    let mut offset = 0;
    while let Some(block) = find_pem_block(&units, offset) {
        let mut replaced: Vec<u16> = Vec::with_capacity(units.len());
        replaced.extend_from_slice(&units[..block.start]);
        replaced.extend(MASK.encode_utf16());
        replaced.extend_from_slice(&units[block.end..]);
        units = replaced;
        offset = block.start + mask_len;
    }
    String::from_utf16_lossy(&units)
}

fn normalized_secret_detect(value: &str) -> bool {
    !normalized_secret_spans(value).is_empty()
}

fn normalized_secret_spans(value: &str) -> Vec<(usize, usize)> {
    let joined = normalize_secret_shadow(value, true);
    let separated = normalize_secret_shadow(value, false);
    let mut spans = secret_spans(&joined);
    let separated_spans = secret_spans(&separated);
    for candidate in separated_spans {
        if !spans
            .iter()
            .any(|matched| matched.0 <= candidate.0 && matched.1 >= candidate.1)
        {
            spans.push(candidate);
        }
    }
    for matched in SPLIT_CREDENTIAL_PATTERN.find_from_ucs2(&joined.units, 0) {
        if let Some(span) = joined.original_span(matched.range().start, matched.range().end) {
            spans.push(span);
        }
    }
    merge_spans(spans)
}

struct SecretShadow {
    units: Vec<u16>,
    byte_starts: Vec<usize>,
    byte_ends: Vec<usize>,
    seams: Vec<usize>,
}

impl SecretShadow {
    fn original_span(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        Some((
            *self.byte_starts.get(start)?,
            *self.byte_ends.get(end.checked_sub(1)?)?,
        ))
    }
}

fn normalize_secret_shadow(value: &str, drop_format: bool) -> SecretShadow {
    let mut shadow = String::new();
    let mut byte_starts = Vec::new();
    let mut byte_ends = Vec::new();
    let mut seams = Vec::new();
    let mut dropped = false;
    for (index, character) in value.char_indices() {
        let control = (character as u32) < 0x20 && character != '\n' && character != '\t';
        let format = matches!(character as u32, 0x00AD | 0x061C | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0xFEFF);
        let end = index + character.len_utf8();
        if character == '\u{00A0}' {
            shadow.push(' ');
            byte_starts.push(index);
            byte_ends.push(end);
            dropped = false;
        } else if (control || format) && drop_format {
            dropped = true;
        } else if control || format {
            shadow.push(' ');
            byte_starts.push(index);
            byte_ends.push(end);
            dropped = false;
        } else {
            if dropped {
                seams.push(shadow.encode_utf16().count());
                dropped = false;
            }
            shadow.push(character);
            for _ in 0..character.len_utf16() {
                byte_starts.push(index);
                byte_ends.push(end);
            }
        }
    }
    SecretShadow { units: shadow.encode_utf16().collect(), byte_starts, byte_ends, seams }
}

fn secret_spans(shadow: &SecretShadow) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    for pattern in SECRET_PATTERNS.iter() {
        for matched in pattern.find_from_ucs2(&shadow.units, 0) {
            let range = matched.range();
            if let Some(span) = shadow.original_span(range.start, range.end) {
                spans.push(span);
            }
        }
        for &seam in &shadow.seams {
            let Some(previous) = seam.checked_sub(1).and_then(|index| shadow.units.get(index)) else { continue };
            let Some(current) = shadow.units.get(seam) else { continue };
            if is_ascii_word_unit(*previous) == is_ascii_word_unit(*current) { continue; }
            let begin = seam.saturating_sub(12);
            for matched in pattern.find_from_ucs2(&shadow.units, begin) {
                let range = matched.range();
                if range.start > seam || range.end < seam { continue; }
                if let Some(span) = shadow.original_span(range.start, range.end) {
                    spans.push(span);
                }
            }
        }
    }
    spans
}

fn is_ascii_word_unit(unit: u16) -> bool {
    (unit >= b'a' as u16 && unit <= b'z' as u16)
        || (unit >= b'A' as u16 && unit <= b'Z' as u16)
        || (unit >= b'0' as u16 && unit <= b'9' as u16)
        || unit == b'_' as u16
}

fn merge_spans(mut spans: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    spans.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for span in spans {
        if let Some(last) = merged.last_mut() && span.0 <= last.1 { last.1 = last.1.max(span.1); }
        else { merged.push(span); }
    }
    merged
}

fn mask_spans(value: &str, spans: Vec<(usize, usize)>) -> String {
    let mut output = String::new();
    let mut cursor = 0;
    for (start, end) in spans {
        output.push_str(&value[cursor..start]);
        output.push_str(MASK);
        cursor = end;
    }
    output.push_str(&value[cursor..]);
    output
}

/// Replace every non-overlapping match of `pattern` in `input` with the mask,
/// mirroring `String.prototype.replace` with a global regular expression.
fn replace_all_ucs2(pattern: &regress::Regex, input: &[u16]) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::with_capacity(input.len());
    let mut last = 0;
    for matched in pattern.find_from_ucs2(input, 0) {
        let range = matched.range();
        out.extend_from_slice(&input[last..range.start]);
        out.extend(MASK.encode_utf16());
        last = range.end;
    }
    out.extend_from_slice(&input[last..]);
    out
}

/// The code unit span of one complete PEM block (upstream `findPemBlock`).
struct PemBlock {
    start: usize,
    end: usize,
}

const PEM_BEGIN: &str = "-----BEGIN ";
const PEM_END: &str = "-----END ";
const PEM_LABEL_MARKER: &str = "-----";
const PEM_LABEL_MAX: usize = 64;

/// Upstream `findPemBlock`: the first `-----BEGIN <label>-----` whose matching
/// `-----END <label>-----` follows, or `None`. Every offset is a UTF-16 code unit
/// index, as JavaScript's `indexOf` and `slice` are.
fn find_pem_block(value: &[u16], from: usize) -> Option<PemBlock> {
    let begin = find_ascii(value, PEM_BEGIN, from)?;
    let label_marker = find_ascii(value, PEM_LABEL_MARKER, begin + PEM_BEGIN.len())?;
    if label_marker - (begin + PEM_BEGIN.len()) > PEM_LABEL_MAX {
        return None;
    }
    let label = &value[begin + PEM_BEGIN.len()..label_marker];
    if label.is_empty() || label.iter().any(|unit| !is_pem_label_unit(*unit)) {
        return None;
    }
    let mut end_marker = find_ascii(value, PEM_END, label_marker + PEM_LABEL_MARKER.len());
    while let Some(marker) = end_marker {
        let end_label_start = marker + PEM_END.len();
        match find_ascii(value, PEM_LABEL_MARKER, end_label_start) {
            Some(end_label_end) if value[end_label_start..end_label_end] == *label => {
                return Some(PemBlock {
                    start: begin,
                    end: end_label_end + PEM_LABEL_MARKER.len(),
                });
            }
            Some(end_label_end) => {
                end_marker = find_ascii(value, PEM_END, end_label_end + PEM_LABEL_MARKER.len());
            }
            None => {
                end_marker = find_ascii(value, PEM_END, end_label_start);
            }
        }
    }
    None
}

/// The ASCII characters upstream's `/[^A-Za-z0-9 ]/` label test admits, as
/// explicit UTF-16 code unit ranges: `A-Z`, `a-z`, `0-9` and space.
fn is_pem_label_unit(unit: u16) -> bool {
    matches!(unit, 0x41..=0x5A | 0x61..=0x7A | 0x30..=0x39 | 0x20)
}

/// `value.indexOf(needle, from)` for an ASCII `needle` over UTF-16 code units.
fn find_ascii(value: &[u16], needle: &str, from: usize) -> Option<usize> {
    let needle = needle.as_bytes();
    if needle.is_empty() || from > value.len() || needle.len() > value.len() - from {
        return None;
    }
    value[from..]
        .windows(needle.len())
        .position(|window| {
            window
                .iter()
                .zip(needle)
                .all(|(unit, byte)| *unit == u16::from(*byte))
        })
        .map(|index| index + from)
}

fn is_scheme_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '+' || character == '.' || character == '-'
}

fn redact_url_userinfo(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    let _bytes = input.as_bytes();

    while let Some(rel_idx) = input[cursor..].find("://") {
        let scheme_delim_idx = cursor + rel_idx;

        let before_delim = &input[..scheme_delim_idx];
        let mut scheme_start = scheme_delim_idx;
        while scheme_start > 0 {
            let prev_char = before_delim[..scheme_start].chars().next_back().unwrap();
            if is_scheme_char(prev_char) {
                scheme_start -= prev_char.len_utf8();
            } else {
                break;
            }
        }

        let scheme_candidate = &input[scheme_start..scheme_delim_idx];
        let valid_scheme = !scheme_candidate.is_empty()
            && scheme_candidate
                .chars()
                .next()
                .unwrap()
                .is_ascii_alphabetic();

        if !valid_scheme {
            out.push_str(&input[cursor..scheme_delim_idx + 3]);
            cursor = scheme_delim_idx + 3;
            continue;
        }

        let after_scheme_idx = scheme_delim_idx + 3;
        let mut userinfo_end = None;
        let mut colon_pos = None;

        for (offset, character) in input[after_scheme_idx..].char_indices() {
            let current_pos = after_scheme_idx + offset;
            if character == '/' || character.is_whitespace() {
                break;
            }
            if character == ':' && colon_pos.is_none() {
                colon_pos = Some(current_pos);
                continue;
            }
            if character == '@' {
                userinfo_end = Some(current_pos);
                break;
            }
        }

        if let Some(at_idx) = userinfo_end {
            let user_part = match colon_pos {
                Some(c_idx) => &input[after_scheme_idx..c_idx],
                None => &input[after_scheme_idx..at_idx],
            };

            if !user_part.is_empty() {
                out.push_str(&input[cursor..scheme_start]);
                out.push_str(&input[scheme_start..after_scheme_idx]);
                if colon_pos.is_some() {
                    out.push_str(MASK);
                    out.push(':');
                    out.push_str(MASK);
                    out.push('@');
                } else {
                    out.push_str(MASK);
                    out.push('@');
                }
                cursor = at_idx + 1;
                continue;
            }
        }

        out.push_str(&input[cursor..after_scheme_idx]);
        cursor = after_scheme_idx;
    }

    out.push_str(&input[cursor..]);
    out
}

fn redact_scp_userinfo(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;

    while let Some(rel_idx) = input[cursor..].find('@') {
        let at_idx = cursor + rel_idx;

        let _before_at = &input[cursor..at_idx];
        let mut user_start = at_idx;
        while user_start > cursor {
            let prev_char = input[..user_start].chars().next_back().unwrap();
            if prev_char.is_whitespace() || prev_char == ':' || prev_char == '/' || prev_char == '@'
            {
                break;
            }
            user_start -= prev_char.len_utf8();
        }

        let user_part = &input[user_start..at_idx];
        if user_part.is_empty() {
            out.push_str(&input[cursor..=at_idx]);
            cursor = at_idx + 1;
            continue;
        }

        let valid_prefix = if user_start == 0 {
            true
        } else {
            let prefix_char = input[..user_start].chars().next_back().unwrap();
            prefix_char.is_whitespace()
                || prefix_char == '\''
                || prefix_char == '"'
                || prefix_char == '('
                || prefix_char == '<'
        };

        if !valid_prefix {
            out.push_str(&input[cursor..=at_idx]);
            cursor = at_idx + 1;
            continue;
        }

        let after_at = &input[at_idx + 1..];
        let mut host_len = 0;
        let mut colon_found = false;

        for (offset, character) in after_at.char_indices() {
            if character == ':' {
                host_len = offset;
                colon_found = true;
                break;
            }
            if character.is_whitespace() || character == '/' || character == '@' {
                break;
            }
        }

        if !colon_found || host_len == 0 {
            out.push_str(&input[cursor..=at_idx]);
            cursor = at_idx + 1;
            continue;
        }

        let colon_idx = at_idx + 1 + host_len;
        let after_colon = &input[colon_idx + 1..];

        let mut has_slash_before_whitespace = false;
        for character in after_colon.chars() {
            if character.is_whitespace() {
                break;
            }
            if character == '/' {
                has_slash_before_whitespace = true;
                break;
            }
        }

        if !has_slash_before_whitespace {
            out.push_str(&input[cursor..=at_idx]);
            cursor = at_idx + 1;
            continue;
        }

        out.push_str(&input[cursor..user_start]);
        out.push_str(MASK);
        out.push('@');
        out.push_str(&input[at_idx + 1..=colon_idx]);
        cursor = colon_idx + 1;
    }

    out.push_str(&input[cursor..]);
    out
}

/// Mask credentials in a URL, or in free text containing URLs.
pub fn redact_url(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let step1 = redact_url_userinfo(value);
    let step2 = redact_scp_userinfo(&step1);
    redact_secret_like_material(&step2)
}

#[cfg(test)]
#[path = "redact_tests.rs"]
mod tests;
