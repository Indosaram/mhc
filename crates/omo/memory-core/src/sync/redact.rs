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

/// True when `value` holds secret-like material (upstream `containsSecretLikeMaterial`).
pub fn contains_secret_like_material(value: &str) -> bool {
    let units: Vec<u16> = value.encode_utf16().collect();
    SECRET_PATTERNS
        .iter()
        .any(|pattern| pattern.find_from_ucs2(&units, 0).next().is_some())
        || find_pem_block(&units, 0).is_some()
}

/// Mask every secret-like pattern in upstream order, then every complete PEM block.
fn redact_secret_like_material(value: &str) -> String {
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
