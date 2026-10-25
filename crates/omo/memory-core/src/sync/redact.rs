//! Secret-like material scanning and credential redaction for memory boundaries.

use std::sync::LazyLock;

const MASK: &str = "***";

/// The classification of a secret-like match. No matched text is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretPatternClass {
    AwsAccessKey,
    CredentialAssignment,
    AuthorizationHeader,
    OpenaiKey,
    VendorToken,
    PemBlock,
    SplitCredentialAssignment,
}

/// Original-input span, in UTF-16 code units like upstream JavaScript indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretMatch {
    pub class: SecretPatternClass,
    pub start: usize,
    pub end: usize,
}

/// Upstream pattern classes, sources and JavaScript flags, in declaration order.
///
/// The patterns are matched through the port's established `regress` UTF-16 seam
/// (`Regex::from_unicode` over UTF-16 code units, then `find_from_ucs2`), which
/// reproduces the non-`u` JavaScript semantics: `\b` is ASCII-only, `\s`
/// includes U+FEFF and excludes U+0085, and value bounds count UTF-16 code units.
pub const SECRET_PATTERN_SOURCES: [(SecretPatternClass, &str, &str); 5] = [
    (
        SecretPatternClass::AwsAccessKey,
        r"\bAKIA[0-9A-Z]{16}\b",
        "",
    ),
    (
        SecretPatternClass::CredentialAssignment,
        r#"\b(?:bearer|token|api[_-]?key|secret|password|passwd|pwd)\s*[=:]\s*(?!(?:process\.env\.[A-Za-z_][A-Za-z0-9_]*|os\.environ(?:\.get\(|\[)?|env\.[A-Za-z_][A-Za-z0-9_]*|\$\{[A-Za-z_][A-Za-z0-9_]*\}|\$[A-Za-z_][A-Za-z0-9_]*|\{\{[^}\n]*\}\}|\[(?:redacted|filtered)\]|(?:redacted|filtered)|(?:bearer|changeme|placeholder|example|none|null|undefined|true|false))(?![^\s"'<>,}\])]))[^\s"'<>,}\])]{1,256}"#,
        "i",
    ),
    (
        SecretPatternClass::AuthorizationHeader,
        r#"\bAuthorization\s*:\s*Bearer\s+(?!(?:process\.env\.[A-Za-z_][A-Za-z0-9_]*|os\.environ(?:\.get\(|\[)?|env\.[A-Za-z_][A-Za-z0-9_]*|\$\{[A-Za-z_][A-Za-z0-9_]*\}|\$[A-Za-z_][A-Za-z0-9_]*|\{\{[^}\n]*\}\}|\[(?:redacted|filtered)\]|(?:redacted|filtered)|(?:bearer|changeme|placeholder|example|none|null|undefined|true|false))(?![^\s"'<>,}\])]))[^\s"'<>,}\])]{1,256}"#,
        "i",
    ),
    (
        SecretPatternClass::OpenaiKey,
        r"\bsk-(?:proj-)?[-_A-Za-z0-9]+\b",
        "",
    ),
    (
        SecretPatternClass::VendorToken,
        r"\b(?:ghp|github_pat|glpat|xox[baprs])[-_][-_A-Za-z0-9]+\b",
        "",
    ),
];
const SPLIT_CREDENTIAL_ASSIGNMENT: &str = r#"(?:a\s*u\s*t\s*h\s*o\s*r\s*i\s*z\s*a\s*t\s*i\s*o\s*n|p\s*a\s*s\s*s\s*w\s*(?:o\s*r\s*)?d|s\s*e\s*c\s*r\s*e\s*t|t\s*o\s*k\s*e\s*n|a\s*p\s*i\s*[_-]?\s*k\s*e\s*y)\s*[:=]\s*(?!(?:process\.env\.[A-Za-z_][A-Za-z0-9_]*|os\.environ(?:\.get\(|\[)?|env\.[A-Za-z_][A-Za-z0-9_]*|\$\{[A-Za-z_][A-Za-z0-9_]*\}|\$[A-Za-z_][A-Za-z0-9_]*|\{\{[^}\n]*\}\}|\[(?:redacted|filtered)\]|(?:redacted|filtered)|(?:bearer|changeme|placeholder|example|none|null|undefined|true|false))(?![^\s"'<>,}\])]))[^\s"'<>,}\])]{6,256}"#;

/// Compiled once. Upstream keeps a non-global and a global copy of every pattern
/// only because a JavaScript `RegExp` carries `lastIndex` state; `regress`
/// matches are stateless, so one set serves both the predicate and the replacement.
static SECRET_PATTERNS: LazyLock<Vec<regress::Regex>> = LazyLock::new(|| {
    SECRET_PATTERN_SOURCES
        .iter()
        .map(|(_, source, flags)| {
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

/// Scan normalized shadows, returning only classes and original UTF-16 spans.
pub fn scan_secret_like_material(value: &str) -> Vec<SecretMatch> {
    let joined = normalize_secret_shadow(value, true);
    let separated = normalize_secret_shadow(value, false);
    let mut matches = Vec::new();
    let mut offset = 0;
    while let Some(block) = find_pem_block(&joined.units, offset) {
        if let Some(span) =
            joined.original_match(SecretPatternClass::PemBlock, block.start, block.end)
        {
            matches.push(span);
        }
        offset = block.end;
    }
    matches.extend(secret_matches(&joined));
    keep_uncovered(&mut matches, secret_matches(&separated));
    keep_uncovered(&mut matches, seam_matches(&joined));
    keep_uncovered(
        &mut matches,
        pattern_matches(
            &joined,
            SecretPatternClass::SplitCredentialAssignment,
            &SPLIT_CREDENTIAL_PATTERN,
        ),
    );
    matches.sort_by_key(|span| (span.start, span.end));
    matches
}

/// True when `value` holds secret-like material (upstream `containsSecretLikeMaterial`).
pub fn contains_secret_like_material(value: &str) -> bool {
    !scan_secret_like_material(value).is_empty()
}

/// Mask the union of every scanner span, preserving surrounding delimiters.
pub fn redact_secret_like_material(value: &str) -> String {
    let spans = merge_spans(
        scan_secret_like_material(value)
            .into_iter()
            .map(|span| (span.start, span.end))
            .collect(),
    );
    if spans.is_empty() {
        return value.to_owned();
    }
    mask_spans(value, spans)
}

struct SecretShadow {
    units: Vec<u16>,
    map: Vec<usize>,
    seams: Vec<usize>,
}

impl SecretShadow {
    fn original_match(
        &self,
        class: SecretPatternClass,
        start: usize,
        end: usize,
    ) -> Option<SecretMatch> {
        Some(SecretMatch {
            class,
            start: *self.map.get(start)?,
            end: self.map.get(end.checked_sub(1)?)? + 1,
        })
    }
}

fn normalize_secret_shadow(value: &str, drop_format: bool) -> SecretShadow {
    let mut units = Vec::new();
    let mut map = Vec::new();
    let mut seams = Vec::new();
    let mut dropped = false;
    let mut index = 0;
    for character in value.chars() {
        let control = u32::from(character) < 0x20 && character != '\n' && character != '\t';
        let format = is_format_character(character);
        if character == '\u{00A0}' || (!drop_format && (control || format)) {
            units.push(u16::from(b' '));
            map.push(index);
        } else if (control || format) && drop_format {
            dropped = true;
        } else {
            if dropped {
                seams.push(units.len());
                dropped = false;
            }
            let mut buffer = [0; 2];
            for (offset, unit) in character.encode_utf16(&mut buffer).iter().enumerate() {
                units.push(*unit);
                map.push(index + offset);
            }
        }
        index += character.len_utf16();
    }
    SecretShadow { units, map, seams }
}

fn is_format_character(character: char) -> bool {
    // Unicode General_Category=Cf, including supplementary format/tag blocks.
    matches!(u32::from(character),
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 |
        0x08E2 | 0x180E | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 |
        0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD |
        0x13430..=0x1343F | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A |
        0xE0001 | 0xE0020..=0xE007F)
}

fn pattern_matches(
    shadow: &SecretShadow,
    class: SecretPatternClass,
    pattern: &regress::Regex,
) -> Vec<SecretMatch> {
    pattern
        .find_from_ucs2(&shadow.units, 0)
        .filter_map(|matched| {
            let range = matched.range();
            shadow.original_match(class, range.start, range.end)
        })
        .collect()
}

fn secret_matches(shadow: &SecretShadow) -> Vec<SecretMatch> {
    SECRET_PATTERN_SOURCES
        .iter()
        .zip(SECRET_PATTERNS.iter())
        .flat_map(|((class, _, _), pattern)| pattern_matches(shadow, *class, pattern))
        .collect()
}

fn keep_uncovered(matches: &mut Vec<SecretMatch>, candidates: Vec<SecretMatch>) {
    let mut sorted: Vec<_> = matches.iter().map(|span| (span.start, span.end)).collect();
    sorted.sort_unstable();
    let mut maximum = 0;
    for (_, end) in &mut sorted {
        maximum = maximum.max(*end);
        *end = maximum;
    }
    for candidate in candidates {
        let index = sorted.partition_point(|(start, _)| *start <= candidate.start);
        if index == 0 || sorted[index - 1].1 < candidate.end {
            matches.push(candidate);
        }
    }
}

struct SeamPattern {
    sticky: regress::Regex,
    whole: regress::Regex,
    boundary: regress::Regex,
    trailing: bool,
    unbounded_tail: bool,
}

static SEAM_PATTERNS: LazyLock<Vec<SeamPattern>> = LazyLock::new(|| {
    SECRET_PATTERN_SOURCES
        .iter()
        .map(|(_, source, flags)| {
            let trailing = source.ends_with(r"\b");
            let body = &source[2..source.len() - if trailing { 2 } else { 0 }];
            let compile = |source: &str| {
                regress::Regex::from_unicode(source.encode_utf16().map(u32::from), *flags)
                    .expect("static seam regex is valid")
            };
            SeamPattern {
                sticky: compile(&format!("^(?:{body})")),
                whole: compile(&format!("^(?:{body})$")),
                boundary: compile(&format!(r"\b{body}")),
                trailing,
                unbounded_tail: body.ends_with("]+"),
            }
        })
        .collect()
});

fn boundary_or_seam(shadow: &SecretShadow, end: usize) -> bool {
    shadow.seams.binary_search(&end).is_ok()
        || end
            .checked_sub(1)
            .and_then(|index| shadow.units.get(index))
            .is_some_and(|unit| is_ascii_word_unit(*unit))
            != shadow
                .units
                .get(end)
                .is_some_and(|unit| is_ascii_word_unit(*unit))
}

fn seam_matches(shadow: &SecretShadow) -> Vec<SecretMatch> {
    let mut found = Vec::new();
    if shadow.seams.is_empty() {
        return found;
    }
    for ((class, _, _), pattern) in SECRET_PATTERN_SOURCES.iter().zip(SEAM_PATTERNS.iter()) {
        let mut starts = shadow.seams.clone();
        if pattern.trailing {
            starts.extend(
                pattern
                    .boundary
                    .find_from_ucs2(&shadow.units, 0)
                    .map(|matched| matched.range().start),
            );
            starts.sort_unstable();
            starts.dedup();
        }
        let mut accepted_end = 0;
        for start in starts {
            if pattern.unbounded_tail && start < accepted_end {
                continue;
            }
            let Some(matched) = pattern
                .sticky
                .find_from_ucs2(&shadow.units[start..], 0)
                .next()
            else {
                continue;
            };
            let mut end = start + matched.range().end;
            if pattern.trailing {
                while end > start && !boundary_or_seam(shadow, end) {
                    end -= 1;
                }
                if end == start
                    || !pattern
                        .whole
                        .find_from_ucs2(&shadow.units[start..end], 0)
                        .any(|matched| matched.range() == (0..end - start))
                {
                    continue;
                }
            }
            if shadow.seams.binary_search(&start).is_err()
                && shadow.seams.binary_search(&end).is_err()
            {
                continue;
            }
            if let Some(span) = shadow.original_match(*class, start, end) {
                found.push(span);
                accepted_end = end;
            }
        }
    }
    found
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
        if let Some(last) = merged.last_mut()
            && span.0 <= last.1
        {
            last.1 = last.1.max(span.1);
        } else {
            merged.push(span);
        }
    }
    merged
}

fn mask_spans(value: &str, spans: Vec<(usize, usize)>) -> String {
    let units: Vec<u16> = value.encode_utf16().collect();
    let mut output = Vec::new();
    let mut cursor = 0;
    for (start, end) in spans {
        output.extend_from_slice(&units[cursor..start]);
        output.extend(MASK.encode_utf16());
        cursor = end;
    }
    output.extend_from_slice(&units[cursor..]);
    String::from_utf16_lossy(&output)
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

static SCP_USERINFO: LazyLock<regress::Regex> = LazyLock::new(|| {
    regress::Regex::from_unicode(
        r#"(^|[\s'"(<])([^\s:/@]{1,256})@([^\s:/@]{1,256}):"#
            .encode_utf16()
            .map(u32::from),
        "",
    )
    .expect("static SCP userinfo regex is valid")
});

fn redact_scp_userinfo(input: &str) -> String {
    let units: Vec<u16> = input.encode_utf16().collect();
    let spans = SCP_USERINFO
        .find_from_ucs2(&units, 0)
        .filter_map(|matched| matched.group(2).map(|range| (range.start, range.end)))
        .collect();
    mask_spans(input, spans)
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

#[cfg(test)]
mod audit_regression_tests {
    use super::*;

    #[test]
    fn scanner_reports_original_utf16_span_when_key_is_obfuscated() {
        let value = "\u{1f600} to\u{e0061}ken=abcdef";
        let matches = scan_secret_like_material(value);
        assert_eq!(
            matches,
            vec![SecretMatch {
                class: SecretPatternClass::CredentialAssignment,
                start: 3,
                end: value.encode_utf16().count(),
            }]
        );
    }

    #[test]
    fn scanner_classifies_each_pattern_when_material_is_present() {
        for (value, class) in [
            ("AKIA1234567890ABCDEF", SecretPatternClass::AwsAccessKey),
            ("token=abcdef", SecretPatternClass::CredentialAssignment),
            ("sk-proj-abcdef", SecretPatternClass::OpenaiKey),
            ("github_pat_abcdef", SecretPatternClass::VendorToken),
            (
                "-----BEGIN A-----x-----END A-----",
                SecretPatternClass::PemBlock,
            ),
            (
                "t o k e n=abcdef",
                SecretPatternClass::SplitCredentialAssignment,
            ),
        ] {
            let matches = scan_secret_like_material(value);
            assert_eq!(
                matches,
                vec![SecretMatch {
                    class,
                    start: 0,
                    end: value.len()
                }]
            );
        }
    }

    #[test]
    fn scanner_keeps_overlapping_classes_when_header_contains_bearer_assignment() {
        let value = "Authorization: Bearer token=abcdef";
        let matches = scan_secret_like_material(value);
        assert_eq!(
            matches,
            vec![
                SecretMatch {
                    class: SecretPatternClass::AuthorizationHeader,
                    start: 0,
                    end: value.len()
                },
                SecretMatch {
                    class: SecretPatternClass::CredentialAssignment,
                    start: 22,
                    end: value.len()
                },
            ]
        );
    }

    #[test]
    fn masking_preserves_delimiters_when_assignments_are_embedded() {
        for (value, expected) in [
            ("see token=abcdef, next", "see ***, next"),
            (r#"{"d":"token=abcdef","n":3}"#, r#"{"d":"***","n":3}"#),
            ("<x a='password=abcdef'>", "<x a='***'>"),
            ("[token=abcdef](x)", "[***](x)"),
            ("(Authorization: Bearer abcdef)", "(***)"),
            ("[t o k e n=abcdef]", "[***]"),
        ] {
            let redacted = redact_secret_like_material(value);
            assert_eq!(redacted, expected);
        }
    }

    #[test]
    fn masking_normalizes_all_format_blocks_when_credentials_are_split() {
        for character in [
            '\u{600}',
            '\u{6dd}',
            '\u{70f}',
            '\u{890}',
            '\u{8e2}',
            '\u{180e}',
            '\u{fff9}',
            '\u{110bd}',
            '\u{110cd}',
            '\u{13430}',
            '\u{1bca0}',
            '\u{1d173}',
            '\u{e0001}',
            '\u{e007f}',
        ] {
            let value = format!("to{character}ken=abcdef");
            let redacted = redact_secret_like_material(&value);
            assert_eq!(redacted, "***");
        }
    }

    #[test]
    fn masking_retains_unassigned_character_when_it_is_not_format() {
        let value = "to\u{2065}ken=abcdef";
        let redacted = redact_secret_like_material(value);
        assert_eq!(redacted, value);
    }

    #[test]
    fn masking_recovers_hidden_boundaries_when_token_has_internal_seams() {
        for (value, expected) in [
            ("prefix\u{200b}sk-proj-ab\u{200b}cdef", "prefix\u{200b}***"),
            (
                "prefix\u{200b}AKIA1234567890ABCDEF\u{200b}suffix",
                "prefix\u{200b}***\u{200b}suffix",
            ),
            ("AKIA1234567890ABCDEF\u{200b}suffix", "***\u{200b}suffix"),
            ("prefix\u{200b}to\u{200b}ken=abcdef", "prefix\u{200b}***"),
        ] {
            let redacted = redact_secret_like_material(value);
            assert_eq!(redacted, expected);
        }
    }

    #[test]
    fn masking_includes_pem_when_other_secrets_are_present() {
        let value = "token=abcdef -----BEGIN PRIVATE KEY-----x-----END PRIVATE KEY-----";
        let redacted = redact_secret_like_material(value);
        assert_eq!(redacted, "*** ***");
    }

    #[test]
    fn masking_includes_normalized_pem_when_marker_is_obfuscated() {
        let value = "note -----BEGIN PRIV\u{200b}ATE KEY-----x-----END PRIVATE KEY----- tail";
        let redacted = redact_secret_like_material(value);
        assert_eq!(redacted, "note *** tail");
    }

    #[test]
    fn predicate_detects_material_when_only_normalized_pem_or_seam_token_exists() {
        for value in [
            "-----BEGIN PRIV\u{13430}ATE KEY-----x-----END PRIVATE KEY-----",
            "prefix\u{200b}AKIA1234567890ABCDEF\u{200b}suffix",
            "ghp_abcdef",
            "glpat_abcdef",
            "xoxb_abcdef",
        ] {
            let detected = contains_secret_like_material(value);
            assert!(detected);
        }
    }

    #[test]
    fn scp_userinfo_is_masked_when_path_has_no_slash() {
        for (value, expected) in [
            ("alice@host:repo", "***@host:repo"),
            ("alice@host:", "***@host:"),
            ("remote 'alice@host:repo'", "remote ***@host:repo'"),
            ("(alice@host:repo)", "***@host:repo)"),
            ("alice@example.com", "alice@example.com"),
            ("/alice@host:repo", "/alice@host:repo"),
        ] {
            let redacted = redact_url(value);
            assert_eq!(redacted, expected);
        }
    }

    #[test]
    fn scp_userinfo_is_not_masked_when_host_exceeds_utf16_bound() {
        let value = format!("alice@{}:repo", "a".repeat(257));
        let redacted = redact_url(&value);
        assert_eq!(redacted, value);
    }
}

#[cfg(test)]
mod reference_value_tests {
    use super::contains_secret_like_material;
    #[test]
    fn references_are_not_credentials() {
        for text in [
            "Authorization: Bearer <key>",
            "client_secret=[FILTERED]",
            r#"JSON.stringify({ password: process.env.TH_PASSWORD || "" })"#,
            "password: os.environ",
            "token: $GITHUB_TOKEN",
            "token = ${GITHUB_TOKEN}",
            "api_key: {{ api_key }}",
            "secret: REDACTED",
            "password: null",
            "token: undefined",
            "secret: example",
            "Authorization: Bearer",
            "token: process.env.GITHUB_TOKEN",
            r#"secret: os.environ["SECRET"]"#,
            "api_key: env.API_KEY",
            "token: {{ secrets.TOKEN }}",
        ] {
            assert!(
                !contains_secret_like_material(text),
                "reference was detected: {text}"
            );
        }
    }
    #[test]
    fn concatenated_literals_still_match() {
        for text in [
            "password: hunter2Abrx9",
            "token=nullable-9fQ2xK7",
            "secret: exampleValue123",
            "api_key: $ecretValue9!x",
            "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.signature",
            "client_secret=abcDEF123456",
            "a p i _ k e y : hunter2Abrx9",
            "sk-proj-AbCdEf123456",
            "AKIAABCDEFGHIJKLMNOP",
            "ghp_Ab3dEf5hJ7kL9mN1pQ3rS5tU7vW9xY1zB3C5",
            "password=process.env.X||hunter2real",
            "token={{x}}realsecretvalue",
            "secret: os.environDEFAULT9xQ",
            "api_key=env.FOO.realtail9xQ",
            "token: ${FOO}realsecret",
            "token: [FILTERED]realsecret",
        ] {
            assert!(
                contains_secret_like_material(text),
                "credential was missed: {text}"
            );
        }
    }
}
