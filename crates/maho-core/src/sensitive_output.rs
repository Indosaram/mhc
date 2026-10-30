//! Port of senpi packages/coding-agent/src/core/sensitive-output.ts.
//!
//! The credential-shape pass (`redact_credential_shapes`) ports senpi-ai (MIT) `utils/secret-redaction.ts`
//! in its shapes-only mode, with the key-prefix and JWT rules of `utils/secret-classifier.ts` and the lexical
//! layer of `utils/secret-lexer.ts` (senpi#4283). The text is read once, left to right, as alternating tokens
//! and delimiters; no backtracking regex is involved. JS indexes strings in UTF-16 units, so the scan runs over
//! UTF-16 units to keep every boundary identical to upstream.

use std::sync::OnceLock;

use regex::Regex;

// An environment assignment whose name contains a credential word. One class up to the "=", anchored at a word
// start, keeps the match linear (senpi#4283); the name test runs once per assignment on that name only.
fn env_assignment() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?-u:\b)([A-Z][A-Z0-9_]*)=([^\s'"]+)"#).expect("env assignment regex")
    })
}

fn env_secret_word() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"API_KEY|SECRET|TOKEN|PASSWORD|AUTH").expect("env secret word regex")
    })
}

fn authorization_bearer() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)(Authorization:\s*Bearer\s+)([^\s'"]+)"#)
            .expect("authorization bearer regex")
    })
}

fn bearer_sk() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(Bearer\s+)(sk-[A-Za-z0-9._-]+)"#).expect("bearer sk regex"))
}

pub fn redact_sensitive_output(text: &str) -> String {
    let redacted = env_assignment()
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let name = &caps[1];
            // `name.slice(1)`: the first letter is never part of the credential-word test.
            if env_secret_word().is_match(&name[1..]) {
                format!("{name}=[REDACTED]")
            } else {
                caps[0].to_string()
            }
        })
        .into_owned();
    let redacted = authorization_bearer()
        .replace_all(&redacted, "$1[REDACTED]")
        .into_owned();
    let redacted = bearer_sk()
        .replace_all(&redacted, "$1[REDACTED]")
        .into_owned();
    redact_sensitive_token_values(&redacted, "[REDACTED]")
}

pub fn redact_sensitive_token_values(text: &str, replacement: &str) -> String {
    redact_credential_shapes(text, replacement)
}

// ---- lexical layer (senpi-ai utils/secret-lexer.ts) ----

const HTML_ENTITIES: [(&str, u16); 10] = [
    ("&quot;", 34),
    ("&#34;", 34),
    ("&#034;", 34),
    ("&#x22;", 34),
    ("&#39;", 39),
    ("&#039;", 39),
    ("&#x27;", 39),
    ("&lt;", 60),
    ("&gt;", 62),
    ("&amp;", 38),
];

const PERCENT_WINDOW: usize = 64;

#[derive(Clone, Copy)]
struct Unit {
    character: u16,
    length: usize,
    /// The unit is an unresolved percent window.
    refusal: bool,
}

fn is_digit(c: u16) -> bool {
    (48..=57).contains(&c)
}
fn is_upper(c: u16) -> bool {
    (65..=90).contains(&c)
}
fn is_lower(c: u16) -> bool {
    (97..=122).contains(&c)
}
fn is_alnum(c: u16) -> bool {
    is_digit(c) || is_upper(c) || is_lower(c)
}
fn is_base64_url(c: u16) -> bool {
    is_alnum(c) || c == 95 || c == 45
}
fn is_name_char(c: u16) -> bool {
    is_base64_url(c) || c == 46
}
fn is_token_char(c: u16) -> bool {
    is_name_char(c) || c == 43 || c == 47 || c == 126
}
fn is_upper_or_digit(c: u16) -> bool {
    is_digit(c) || is_upper(c)
}
fn is_slack_char(c: u16) -> bool {
    is_alnum(c) || c == 45
}
fn is_pat_char(c: u16) -> bool {
    is_alnum(c) || c == 95
}
fn is_word_char(c: u16) -> bool {
    is_alnum(c) || c == 95
}

fn hex_value(c: u16) -> Option<u16> {
    match c {
        48..=57 => Some(c - 48),
        65..=70 => Some(c - 55),
        97..=102 => Some(c - 87),
        _ => None,
    }
}

fn matches_at(text: &[u16], i: usize, literal: &str, limit: usize) -> bool {
    let bytes = literal.as_bytes();
    if i + bytes.len() > limit {
        return false;
    }
    bytes
        .iter()
        .enumerate()
        .all(|(k, b)| text[i + k] == u16::from(*b))
}

/// The length of the token unit at `i` (a value character, or a percent escape of one), or 0 at a delimiter.
fn token_unit_length(text: &[u16], i: usize) -> usize {
    let c = text[i];
    if is_token_char(c) {
        return 1;
    }
    if c != 37 {
        return 0;
    }
    let unit = read_unit(text, i);
    if unit.refusal {
        return 0;
    }
    if is_token_char(unit.character) || unit.length == 1 {
        unit.length
    } else {
        0
    }
}

/// Repeated full-window percent decoding to a fixpoint, keeping each character's original source end.
fn percent_window(text: &[u16], start: usize, prefix_end: usize) -> Unit {
    let limit = text.len().min(prefix_end + PERCENT_WINDOW - 1);
    let mut needs_decoding = false;
    let mut contiguous = true;
    let mut previous = 37u16;
    for &c in text.iter().take(limit).skip(prefix_end) {
        let hex = hex_value(c).is_some();
        if previous == 37 && hex {
            needs_decoding = true;
            break;
        }
        if c != 37 && !hex {
            contiguous = false;
        }
        previous = c;
    }
    if !needs_decoding {
        if contiguous && limit < text.len() {
            return Unit {
                character: 37,
                length: limit - start,
                refusal: true,
            };
        }
        return Unit {
            character: 37,
            length: prefix_end - start,
            refusal: false,
        };
    }
    let mut characters: Vec<u16> = vec![37];
    let mut ends: Vec<usize> = vec![prefix_end];
    for (p, &c) in text.iter().enumerate().take(limit).skip(prefix_end) {
        characters.push(c);
        ends.push(p + 1);
    }
    loop {
        let mut changed = false;
        let mut written = 0usize;
        let mut p = 0usize;
        while p < characters.len() {
            let mut c = characters[p];
            let high = characters.get(p + 1).copied().and_then(hex_value);
            let low = characters.get(p + 2).copied().and_then(hex_value);
            if c == 37
                && let (Some(high), Some(low)) = (high, low)
            {
                c = high * 16 + low;
                p += 2;
                changed = true;
            }
            characters[written] = c;
            ends[written] = ends[p];
            written += 1;
            p += 1;
        }
        characters.truncate(written);
        ends.truncate(written);
        if !changed {
            break;
        }
    }
    let character = characters[0];
    if character == 37 && limit < text.len() {
        // Only a contiguous percent/hex run can depend on text beyond the window.
        for &c in characters.iter().skip(1) {
            if c != 37 && hex_value(c).is_none() {
                return Unit {
                    character,
                    length: ends[0] - start,
                    refusal: false,
                };
            }
        }
        return Unit {
            character,
            length: limit - start,
            refusal: true,
        };
    }
    Unit {
        character,
        length: ends[0] - start,
        refusal: false,
    }
}

/// One logical character and its full source width.
fn read_unit(text: &[u16], i: usize) -> Unit {
    let c = text[i];
    if c == 37 {
        let mut end = i + 1;
        // Collapse literal %25 layers in one forward walk before the bounded full-window decoder.
        while end + 1 < text.len() {
            let (Some(high), Some(low)) = (hex_value(text[end]), hex_value(text[end + 1])) else {
                break;
            };
            let decoded = high * 16 + low;
            end += 2;
            if decoded != 37 {
                return Unit {
                    character: decoded,
                    length: end - i,
                    refusal: false,
                };
            }
        }
        return percent_window(text, i, end);
    }
    if c == 38 {
        for (entity, character) in HTML_ENTITIES {
            if matches_at(text, i, entity, text.len()) {
                return Unit {
                    character,
                    length: entity.len(),
                    refusal: false,
                };
            }
        }
    }
    let character = match c {
        0xff1a => 58,
        0x201c | 0x201d => 34,
        0x2018 | 0x2019 => 39,
        other => other,
    };
    Unit {
        character,
        length: 1,
        refusal: false,
    }
}

/// The delimiter that starts at `i`, as far as the shape pass reads it: its end, and where a bounded
/// percent-decoder refusal begins (the end is then the text's).
fn read_delimiter_end(text: &[u16], i: usize) -> (usize, Option<usize>) {
    let mut position = i;
    while position < text.len() && token_unit_length(text, position) == 0 {
        let unit = read_unit(text, position);
        if unit.refusal {
            return (text.len(), Some(position));
        }
        position += unit.length;
    }
    (position, None)
}

// ---- credential shapes (senpi-ai utils/secret-classifier.ts, utils/secret-redaction.ts) ----

struct PrefixRule {
    prefixes: &'static [&'static str],
    min: usize,
    chars: fn(u16) -> bool,
    exact: bool,
}

const PREFIX_RULES: [PrefixRule; 8] = [
    PrefixRule {
        prefixes: &["sk-"],
        min: 8,
        chars: is_base64_url,
        exact: false,
    },
    PrefixRule {
        prefixes: &["sk_live_", "sk_test_", "rk_live_", "rk_test_"],
        min: 16,
        chars: is_alnum,
        exact: false,
    },
    PrefixRule {
        prefixes: &["gsk_", "xai-", "hf_"],
        min: 20,
        chars: is_alnum,
        exact: false,
    },
    PrefixRule {
        prefixes: &["xoxa-", "xoxb-", "xoxo-", "xoxp-", "xoxr-", "xoxs-"],
        min: 10,
        chars: is_slack_char,
        exact: false,
    },
    PrefixRule {
        prefixes: &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"],
        min: 20,
        chars: is_alnum,
        exact: false,
    },
    PrefixRule {
        prefixes: &["github_pat_"],
        min: 20,
        chars: is_pat_char,
        exact: false,
    },
    PrefixRule {
        prefixes: &["AKIA", "ASIA"],
        min: 16,
        chars: is_upper_or_digit,
        exact: true,
    },
    PrefixRule {
        prefixes: &["AIza"],
        min: 30,
        chars: is_base64_url,
        exact: false,
    },
];

/// Bounded work: the prefix plus at most min + 1 characters after it.
fn matches_prefix_rule(text: &[u16], start: usize, end: usize, rule: &PrefixRule) -> bool {
    for prefix in rule.prefixes {
        if !matches_at(text, start, prefix, end) {
            continue;
        }
        let mut run = 0usize;
        let limit = if rule.exact { rule.min + 1 } else { rule.min };
        let mut i = start + prefix.len();
        while i < end && run < limit && (rule.chars)(text[i]) {
            run += 1;
            i += 1;
        }
        return if rule.exact {
            run == rule.min
        } else {
            run >= rule.min
        };
    }
    false
}

/// A known key ends at its alphabet's boundary, not at the end of a URL or filename containing it.
fn credential_shape_end(text: &[u16], start: usize, end: usize) -> usize {
    for rule in &PREFIX_RULES {
        if !matches_prefix_rule(text, start, end, rule) {
            continue;
        }
        for prefix in rule.prefixes {
            if !matches_at(text, start, prefix, end) {
                continue;
            }
            let mut i = start + prefix.len();
            while i < end {
                let unit = read_unit(text, i);
                if !(rule.chars)(unit.character) {
                    break;
                }
                i += unit.length;
            }
            return i;
        }
    }
    // JWTs have their own segment grammar; keep the conservative span for those.
    end
}

/// Whether a JWT ("eyJ" header, three base64url segments) starts at `p`: `None` when one does, else where the
/// next candidate may start.
fn jwt_at(text: &[u16], p: usize, end: usize) -> Option<usize> {
    if !matches_at(text, p, "eyJ", end) {
        return Some(p + 1);
    }
    let mut segments = 1;
    let mut length = 0usize;
    let mut first_dot: Option<usize> = None;
    let mut i = p;
    while i < end {
        let unit = read_unit(text, i);
        let c = unit.character;
        if c == 46 {
            let dot = *first_dot.get_or_insert(i);
            let next = if i + unit.length < end {
                read_unit(text, i + unit.length).character
            } else {
                0
            };
            if segments == 3 {
                return if length >= 5 && !is_base64_url(next) {
                    None
                } else {
                    Some(dot)
                };
            }
            if length < if segments == 1 { 8 } else { 5 } {
                return Some(dot);
            }
            segments += 1;
            length = 0;
        } else if is_base64_url(c) {
            length += 1;
        } else {
            break;
        }
        i += unit.length;
    }
    if segments == 3 && length >= 5 {
        return None;
    }
    Some(first_dot.unwrap_or(i))
}

/// Where the first credential-shaped run in `[start, end)` begins; `within_credential` also checks inside words.
fn credential_shape_start(
    text: &[u16],
    start: usize,
    end: usize,
    within_credential: bool,
) -> Option<usize> {
    let mut stop = end;
    while stop > start && text[stop - 1] == 46 {
        stop -= 1;
    }
    let mut jwt_from = start;
    let mut word_start = true;
    let mut p = start;
    while p < stop {
        let unit = read_unit(text, p);
        let candidate = within_credential || word_start;
        word_start = !is_word_char(unit.character);
        let position = p;
        p += unit.length;
        if !candidate {
            continue;
        }
        if PREFIX_RULES
            .iter()
            .any(|rule| matches_prefix_rule(text, position, stop, rule))
        {
            return Some(position);
        }
        if position < jwt_from {
            continue;
        }
        match jwt_at(text, position, stop) {
            None => return Some(position),
            Some(next) => jwt_from = next,
        }
    }
    None
}

/// Append `[start, end)` to the sorted, disjoint ranges, merging what it overlaps.
fn add_range(ranges: &mut Vec<usize>, start: usize, end: usize) {
    let mut stop = end;
    while ranges.len() >= 2 && ranges[ranges.len() - 2] >= start {
        stop = stop.max(ranges[ranges.len() - 1]);
        ranges.truncate(ranges.len() - 2);
    }
    let extends_last = ranges.last().is_some_and(|last| *last >= start);
    if extends_last {
        let index = ranges.len() - 1;
        ranges[index] = ranges[index].max(stop);
    } else {
        ranges.push(start);
        ranges.push(stop);
    }
}

fn shape_ranges(text: &[u16]) -> Vec<usize> {
    let mut ranges = Vec::new();
    let (mut i, leading_refusal) = read_delimiter_end(text, 0);
    if let Some(refusal) = leading_refusal {
        return vec![refusal, text.len()];
    }
    while i < text.len() {
        let start = i;
        while i < text.len() {
            let length = token_unit_length(text, i);
            if length == 0 {
                break;
            }
            i += length;
        }
        let end = i;
        let (after_end, refusal) = read_delimiter_end(text, end);
        i = after_end;
        if refusal.is_some() {
            // Do not let a bounded decoder refusal expose an earlier shape.
            add_range(&mut ranges, start, text.len());
            break;
        }
        let Some(shape) = credential_shape_start(text, start, end, false) else {
            continue;
        };
        // A second prefix can begin inside the first alphabet; keep the whole token for joined credentials.
        let joined = credential_shape_start(text, shape + read_unit(text, shape).length, end, true);
        let stop = if joined.is_some() {
            end
        } else {
            credential_shape_end(text, shape, end)
        };
        add_range(&mut ranges, shape, stop);
    }
    ranges
}

/// Mask only credential-shaped runs (known key prefixes and JWTs), whatever surrounds them. One linear pass.
pub fn redact_credential_shapes(text: &str, replacement: &str) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let ranges = shape_ranges(&units);
    if ranges.is_empty() {
        return text.to_string();
    }
    let mut out = String::new();
    let mut copied = 0usize;
    for pair in ranges.chunks_exact(2) {
        out.push_str(&String::from_utf16_lossy(&units[copied..pair[0]]));
        out.push_str(replacement);
        copied = pair[1];
    }
    out.push_str(&String::from_utf16_lossy(&units[copied..]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_env_assignments() {
        assert_eq!(
            redact_sensitive_output("OPENAI_API_KEY=sk-live-abc"),
            "OPENAI_API_KEY=[REDACTED]"
        );
        assert_eq!(
            redact_sensitive_output("GITHUB_TOKEN=xyz"),
            "GITHUB_TOKEN=[REDACTED]"
        );
    }

    #[test]
    fn redacts_authorization_bearer_headers() {
        assert_eq!(
            redact_sensitive_output("Authorization: Bearer abc.def"),
            "Authorization: Bearer [REDACTED]"
        );
        assert_eq!(
            redact_sensitive_output("authorization: bearer abc"),
            "authorization: bearer [REDACTED]"
        );
    }

    #[test]
    fn redacts_bearer_sk_tokens() {
        assert_eq!(
            redact_sensitive_output("Bearer sk-1234567890"),
            "Bearer [REDACTED]"
        );
    }

    #[test]
    fn redacts_github_tokens() {
        assert_eq!(
            redact_sensitive_output("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
            "[REDACTED]"
        );
        assert_eq!(
            redact_sensitive_output("github_pat_abcdefghijklmnopqrstuv"),
            "[REDACTED]"
        );
    }

    #[test]
    fn leaves_ordinary_text_untouched() {
        assert_eq!(
            redact_sensitive_output("just a normal line"),
            "just a normal line"
        );
    }

    #[test]
    fn a_custom_replacement_is_honored() {
        assert_eq!(
            redact_sensitive_token_values("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ", "<gone>"),
            "<gone>"
        );
    }

    #[test]
    fn env_assignment_needs_a_credential_word_after_the_first_letter() {
        assert_eq!(
            redact_sensitive_output("MY_SECRET_VALUE=abc"),
            "MY_SECRET_VALUE=[REDACTED]"
        );
        assert_eq!(redact_sensitive_output("PATH=/usr/bin"), "PATH=/usr/bin");
        assert_eq!(redact_sensitive_output("SECRET=abc"), "SECRET=abc");
        assert_eq!(redact_sensitive_output("XSECRET=abc"), "XSECRET=[REDACTED]");
    }

    #[test]
    fn long_env_name_stays_linear() {
        let text = format!("{}=value", "A".repeat(200_000));
        assert_eq!(redact_sensitive_output(&text), text);
    }

    #[test]
    fn redacts_credential_shapes_beyond_github_tokens() {
        let r = |t: &str| redact_credential_shapes(t, "[X]");
        assert_eq!(r("key sk-abcdefgh12345 end"), "key [X] end");
        assert_eq!(r("id AKIAABCDEFGHIJKLMNOP."), "id [X].");
        assert_eq!(r("AKIAABCDEFGHIJKLMNOPQ"), "AKIAABCDEFGHIJKLMNOPQ");
        assert_eq!(r("gho_ABCDEFGHIJKLMNOPQRSTUV"), "[X]");
        assert_eq!(r("xoxb-1234567890-abc"), "[X]");
        assert_eq!(r("AIza0123456789012345678901234567890"), "[X]");
        assert_eq!(r("eyJhbGciOiJIUzI1.eyJzdWIxMjM.c2lnbmF0dXJl"), "[X]");
    }

    #[test]
    fn shapes_only_match_at_a_word_start() {
        let r = |t: &str| redact_credential_shapes(t, "[X]");
        assert_eq!(r("disk-abcdefghijkl"), "disk-abcdefghijkl");
        assert_eq!(r("https://h/path/sk-abcdefgh1234"), "https://h/path/[X]");
        assert_eq!(r("sk-ab"), "sk-ab");
    }

    #[test]
    fn percent_encoded_prefix_is_read_as_what_it_encodes() {
        let r = |t: &str| redact_credential_shapes(t, "[X]");
        assert_eq!(r("x%3Dsk-abcdefgh1234"), "x%3D[X]");
        assert_eq!(r("a=%2"), "a=%2");
    }

    #[test]
    fn non_ascii_text_survives_redaction() {
        let r = |t: &str| redact_credential_shapes(t, "[X]");
        assert_eq!(
            r("caf\u{e9} \u{1f600} sk-abcdefgh1234 \u{1f600}"),
            "caf\u{e9} \u{1f600} [X] \u{1f600}"
        );
    }
}
