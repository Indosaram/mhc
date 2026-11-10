//! Secret-like material: detection and redaction before a memory is written.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-redact.md`. Memory is long-lived and shared across
//! sessions, so a credential that reaches it leaks forever. Every candidate value is scanned first
//! and any match is replaced by `***` before the text is stored. Spans are byte offsets into the
//! original string, which is what Rust string slicing needs.

/// What kind of secret-like material a span holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SecretClass {
    /// An AWS access key id (`AKIA…`).
    AwsAccessKey,
    /// A `key = value` or `key: value` assignment to a credential name.
    CredentialAssignment,
    /// An `Authorization: Bearer …` header.
    AuthorizationHeader,
    /// An OpenAI-style `sk-…` key.
    OpenaiKey,
    /// A vendor token (`ghp_`, `github_pat_`, `glpat-`, `xox…`).
    VendorToken,
    /// A PEM private-key block.
    PemBlock,
    /// The same assignment with the credential name spelled out between spaces.
    SplitCredentialAssignment,
}

/// One match: its class and the byte span in the original input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecretMatch {
    /// What was matched.
    pub class: SecretClass,
    /// Start byte offset, inclusive.
    pub start: usize,
    /// End byte offset, exclusive.
    pub end: usize,
}

impl SecretMatch {
    /// The slice of `value` this match covers.
    pub fn slice<'a>(&self, value: &'a str) -> &'a str {
        &value[self.start..self.end]
    }
}

/// The replacement written in place of a match.
pub const MASK: &str = "***";

const CREDENTIAL_NAMES: [&str; 7] = [
    "token", "secret", "password", "passwd", "pwd", "apikey", "bearer",
];
const SPLIT_CREDENTIAL_NAMES: [&str; 6] = [
    "authorization",
    "password",
    "passwd",
    "secret",
    "token",
    "apikey",
];
const PLACEHOLDERS: [&str; 9] = [
    "bearer",
    "changeme",
    "placeholder",
    "example",
    "none",
    "null",
    "undefined",
    "true",
    "false",
];

/// True when `value` holds secret-like material.
pub fn contains_secret_like_material(value: &str) -> bool {
    !scan_secret_like_material(value).is_empty()
}

/// Every secret-like span in `value`, sorted by position and never overlapping.
pub fn scan_secret_like_material(value: &str) -> Vec<SecretMatch> {
    let bytes = value.as_bytes();
    let mut found = Vec::new();
    collect_pem_blocks(value, &mut found);
    collect_token_prefixes(value, &mut found);
    collect_authorization_headers(value, &mut found);
    collect_credential_assignments(bytes, &mut found);
    collect_split_credential_assignments(bytes, &mut found);
    found.sort_by_key(|entry| (entry.start, std::cmp::Reverse(entry.end)));
    keep_uncovered(found)
}

/// Replace every secret-like span with [`MASK`].
///
/// The mask is three characters and the surrounding text is preserved exactly, so redacting an
/// already-redacted value changes nothing.
pub fn redact_secret_like_material(value: &str) -> String {
    let spans = scan_secret_like_material(value);
    if spans.is_empty() {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    let mut cursor = 0usize;
    for span in spans {
        out.push_str(&value[cursor..span.start]);
        out.push_str(MASK);
        cursor = span.end;
    }
    out.push_str(&value[cursor..]);
    out
}

/// The secret classes present in `value`, without spans.
pub fn secret_classes(value: &str) -> Vec<SecretClass> {
    let mut classes: Vec<SecretClass> = Vec::new();
    for entry in scan_secret_like_material(value) {
        if !classes.contains(&entry.class) {
            classes.push(entry.class);
        }
    }
    classes
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn eq_ignore_ascii(value: &str, expected: &str) -> bool {
    value.len() == expected.len() && value.eq_ignore_ascii_case(expected)
}

fn starts_with_ignore_ascii(value: &str, prefix: &str) -> bool {
    value.len() >= prefix.len() && value[..prefix.len()].eq_ignore_ascii_case(prefix)
}

fn is_value_delimiter(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>' | ',' | '}' | ']' | ')' | '`')
}

fn is_placeholder(value: &str) -> bool {
    let lowered = value.to_ascii_lowercase();
    if PLACEHOLDERS.iter().any(|known| *known == lowered) {
        return true;
    }
    if lowered == "redacted" || lowered == "filtered" {
        return true;
    }
    starts_with_ignore_ascii(value, "process.env.")
        || starts_with_ignore_ascii(value, "os.environ")
        || starts_with_ignore_ascii(value, "env.")
        || value.starts_with("${")
        || value.starts_with('$')
        || value.starts_with("{{")
}

fn collect_pem_blocks(value: &str, found: &mut Vec<SecretMatch>) {
    let mut offset = 0usize;
    while let Some(begin) = value[offset..].find("-----BEGIN ") {
        let begin = offset + begin;
        let Some(head) = value[begin..].find("PRIVATE KEY-----") else {
            break;
        };
        let head_end = begin + head + "PRIVATE KEY-----".len();
        let Some(tail) = value[head_end..].find("-----END ") else {
            offset = head_end;
            continue;
        };
        let tail_start = head_end + tail;
        let Some(tail_len) = value[tail_start..].find("PRIVATE KEY-----") else {
            offset = head_end;
            continue;
        };
        let end = tail_start + tail_len + "PRIVATE KEY-----".len();
        found.push(SecretMatch {
            class: SecretClass::PemBlock,
            start: begin,
            end,
        });
        offset = end;
    }
}

fn collect_token_prefixes(value: &str, found: &mut Vec<SecretMatch>) {
    let mut previous: Option<char> = None;
    for (start, ch) in value.char_indices() {
        let previous_is_word = previous.is_some_and(is_word_char);
        previous = Some(ch);
        if previous_is_word {
            continue;
        }
        let rest = &value[start..];
        let class = if rest.starts_with("AKIA") {
            let body = rest.get(4..20).unwrap_or_default();
            if body.len() == 16
                && body
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase())
            {
                Some((SecretClass::AwsAccessKey, 20))
            } else {
                None
            }
        } else if rest.starts_with("sk-") {
            match token_run(rest) {
                Some(len) if len > 3 => Some((SecretClass::OpenaiKey, len)),
                _ => None,
            }
        } else if [
            "ghp_",
            "github_pat_",
            "glpat-",
            "xoxb-",
            "xoxp-",
            "xoxa-",
            "xoxr-",
            "xoxs-",
        ]
        .iter()
        .any(|prefix| rest.starts_with(prefix))
        {
            match token_run(rest) {
                Some(len) if len > 5 => Some((SecretClass::VendorToken, len)),
                _ => None,
            }
        } else {
            None
        };
        if let Some((class, len)) = class {
            found.push(SecretMatch {
                class,
                start,
                end: start + len,
            });
        }
    }
}

fn token_run(value: &str) -> Option<usize> {
    let mut end = 0usize;
    for (offset, ch) in value.char_indices() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            end = offset + ch.len_utf8();
        } else {
            break;
        }
    }
    (end > 0).then_some(end)
}

fn collect_authorization_headers(value: &str, found: &mut Vec<SecretMatch>) {
    let needle = "authorization";
    for (start, _) in value.char_indices() {
        let Some(head) = value.get(start..).and_then(|rest| rest.get(..needle.len())) else {
            continue;
        };
        if !eq_ignore_ascii(head, needle) {
            continue;
        }
        let Some(after) = start.checked_add(needle.len()) else {
            continue;
        };
        let Some(tail) = value.get(after..) else {
            continue;
        };
        let Some(colon) = tail.find(':') else {
            continue;
        };
        let mut cursor = after + colon + 1;
        while value
            .get(cursor..)
            .is_some_and(|rest| rest.starts_with(' '))
        {
            cursor += 1;
        }
        let Some(bearer) = value.get(cursor..).and_then(|rest| rest.get(..6)) else {
            continue;
        };
        if !eq_ignore_ascii(bearer, "bearer") {
            continue;
        }
        cursor += 6;
        while value
            .get(cursor..)
            .is_some_and(|rest| rest.starts_with(' '))
        {
            cursor += 1;
        }
        let Some(len) = value.get(cursor..).and_then(token_run) else {
            continue;
        };
        let candidate = &value[cursor..cursor + len];
        if is_placeholder(candidate) {
            continue;
        }
        found.push(SecretMatch {
            class: SecretClass::AuthorizationHeader,
            start,
            end: cursor + len,
        });
    }
}

fn collect_credential_assignments(bytes: &[u8], found: &mut Vec<SecretMatch>) {
    let value = std::str::from_utf8(bytes).unwrap_or_default();
    let mut start = 0usize;
    while start < bytes.len() {
        let Some(name) = solid_credential_name_at(value, start) else {
            start += 1;
            continue;
        };
        if let Some(end) = assignment_value_end(value, name) {
            found.push(SecretMatch {
                class: SecretClass::CredentialAssignment,
                start,
                end,
            });
        }
        start = name;
    }
}

fn solid_credential_name_at(value: &str, start: usize) -> Option<usize> {
    let bytes = value.as_bytes();
    if start > 0 && is_word_byte(bytes[start - 1]) {
        return None;
    }
    let mut end = start;
    while end < bytes.len() && is_word_byte(bytes[end]) {
        end += 1;
    }
    if end == start {
        return None;
    }
    let token = &value[start..end];
    let normalized = token.replace(['-', '_'], "").to_ascii_lowercase();
    CREDENTIAL_NAMES
        .contains(&normalized.as_str())
        .then_some(end)
}

fn collect_split_credential_assignments(bytes: &[u8], found: &mut Vec<SecretMatch>) {
    let value = std::str::from_utf8(bytes).unwrap_or_default();
    for (start, _) in value.char_indices() {
        let previous_is_word = value[..start].chars().next_back().is_some_and(is_word_char);
        if previous_is_word {
            continue;
        }
        let Some(end) = split_credential_name_end(value, start) else {
            continue;
        };
        if let Some(span_end) = assignment_value_end(value, end) {
            found.push(SecretMatch {
                class: SecretClass::SplitCredentialAssignment,
                start,
                end: span_end,
            });
        }
    }
}

fn split_credential_name_end(value: &str, start: usize) -> Option<usize> {
    for name in SPLIT_CREDENTIAL_NAMES {
        let mut cursor = start;
        let mut matched = 0usize;
        for expected in name.chars() {
            let Some(ch) = value[cursor..].chars().next() else {
                break;
            };
            if ch.to_ascii_lowercase() != expected {
                break;
            }
            cursor += ch.len_utf8();
            matched += 1;
            if matched < name.len() {
                let mut peek = cursor;
                let mut spaces = 0usize;
                while value[peek..].starts_with(' ') {
                    peek += 1;
                    spaces += 1;
                }
                if spaces > 0 {
                    cursor = peek;
                }
            }
        }
        if matched == name.len() {
            return Some(cursor);
        }
    }
    None
}

fn assignment_value_end(value: &str, name_end: usize) -> Option<usize> {
    let mut cursor = name_end;
    while value[cursor..].starts_with([' ', '\t']) {
        cursor += 1;
    }
    let separator = value[cursor..].chars().next()?;
    if separator != '=' && separator != ':' {
        return None;
    }
    cursor += separator.len_utf8();
    while value[cursor..].starts_with([' ', '\t']) {
        cursor += 1;
    }
    let rest = &value[cursor..];
    let (len, close) = match rest.chars().next()? {
        quote @ ('"' | '\'') => {
            let body = &rest[quote.len_utf8()..];
            let end = body.find(quote)?;
            (quote.len_utf8() + end + quote.len_utf8(), true)
        }
        '[' => {
            let end = rest.find(']')?;
            (end + 1, true)
        }
        _ => {
            let mut len = 0usize;
            for ch in rest.chars() {
                if is_value_delimiter(ch) {
                    break;
                }
                len += ch.len_utf8();
            }
            (len, false)
        }
    };
    if len == 0 || len > 256 {
        return None;
    }
    let candidate = &rest[..len];
    let bare = if close {
        candidate.trim_matches(['"', '\'', '[', ']'])
    } else {
        candidate
    };
    if is_placeholder(bare) {
        return None;
    }
    Some(cursor + len)
}

fn keep_uncovered(mut spans: Vec<SecretMatch>) -> Vec<SecretMatch> {
    let mut kept: Vec<SecretMatch> = Vec::with_capacity(spans.len());
    for span in spans.drain(..) {
        match kept.last_mut() {
            Some(last) if span.start < last.end => {
                if span.end > last.end {
                    last.end = span.end;
                }
            }
            _ => kept.push(span),
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redacted(value: &str) -> String {
        redact_secret_like_material(value)
    }

    #[test]
    fn an_aws_key_is_masked_and_the_surrounding_text_survives() {
        let text = "use AKIAIOSFODNN7EXAMPLE for the bucket";
        assert_eq!(redacted(text), "use *** for the bucket");
        assert!(contains_secret_like_material(text));
    }

    #[test]
    fn an_aws_shaped_token_of_the_wrong_length_is_not_a_key() {
        assert!(!contains_secret_like_material("AKIASHORT"));
        assert!(!contains_secret_like_material("AKIAIOSFODNN7EXAMPL"));
    }

    #[test]
    fn a_credential_assignment_is_masked_whole() {
        let text = "password = hunter2!";
        assert_eq!(redacted(text), "***");
        assert_eq!(
            scan_secret_like_material(text)[0].class,
            SecretClass::CredentialAssignment
        );
    }

    #[test]
    fn a_colon_assignment_is_matched_too() {
        let text = "config: api_key: abc123def";
        assert!(redacted(text).contains(MASK));
        assert!(!redacted(text).contains("abc123def"));
    }

    #[test]
    fn placeholders_and_environment_references_are_left_alone() {
        for text in [
            "password: changeme",
            "token: process.env.GITHUB_TOKEN",
            "secret: ${SECRET}",
            "api_key: {{ key }}",
            "pwd: [redacted]",
            "token: none",
        ] {
            assert!(
                !contains_secret_like_material(text),
                "should not match: {text}"
            );
        }
    }

    #[test]
    fn an_authorization_header_is_masked() {
        let text = "Authorization: Bearer abcdef123456";
        assert_eq!(redacted(text), "***");
        assert_eq!(
            scan_secret_like_material(text)[0].class,
            SecretClass::AuthorizationHeader
        );
    }

    #[test]
    fn an_authorization_header_with_a_placeholder_is_left_alone() {
        assert!(!contains_secret_like_material(
            "Authorization: Bearer ${TOKEN}"
        ));
    }

    #[test]
    fn an_openai_key_is_masked() {
        let text = "OPENAI=sk-proj-abc123DEF456";
        let out = redacted(text);
        assert!(!out.contains("sk-proj"));
        assert!(out.contains(MASK));
    }

    #[test]
    fn a_github_token_is_masked() {
        let text = "ghp_abcdefghijklmnopqrstuvwxyz0123";
        assert_eq!(redacted(text), MASK);
        assert_eq!(
            scan_secret_like_material(text)[0].class,
            SecretClass::VendorToken
        );
    }

    #[test]
    fn a_pem_private_key_block_is_masked_as_one_span() {
        let text =
            "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----\nafter";
        assert_eq!(redacted(text), "before\n***\nafter");
        assert_eq!(
            scan_secret_like_material(text)[0].class,
            SecretClass::PemBlock
        );
    }

    #[test]
    fn a_split_credential_name_is_still_caught() {
        let text = "a u t h o r i z a t i o n : abc123456";
        let out = redacted(text);
        assert!(!out.contains("abc123456"));
        assert_eq!(
            scan_secret_like_material(text)[0].class,
            SecretClass::SplitCredentialAssignment
        );
    }

    #[test]
    fn the_word_secret_without_an_assignment_is_not_a_secret() {
        assert!(!contains_secret_like_material("this is a secret project"));
        assert!(!contains_secret_like_material(
            "tokens are stored in the bank"
        ));
    }

    #[test]
    fn offsets_slice_back_to_the_matched_text() {
        let text = "prefix ghp_abcdefghijklmnop suffix";
        let span = scan_secret_like_material(text)[0];
        assert_eq!(span.slice(text), "ghp_abcdefghijklmnop");
    }

    #[test]
    fn redaction_is_idempotent() {
        let text = "token: abc123 and ghp_abcdefghijklmnop";
        let once = redacted(text);
        assert_eq!(redacted(&once), once);
    }

    #[test]
    fn scanning_a_non_ascii_value_does_not_panic() {
        for text in [
            "한국어 메모리에 저장된 내용",
            "🚀🚀🚀",
            "한국어 ghp_abcdefghijklmnop 뒤",
            "앞 AKIAIOSFODNN7EXAMPLE 뒤",
            "authorization: bearer 한국어토큰",
        ] {
            let _ = scan_secret_like_material(text);
            let _ = redacted(text);
            let _ = secret_classes(text);
        }
    }

    #[test]
    fn a_secret_after_non_ascii_text_is_still_masked() {
        let out = redacted("한국어 ghp_abcdefghijklmnop");
        assert!(!out.contains("ghp_"));
        assert!(out.contains(MASK));
    }

    #[test]
    fn several_secrets_in_one_value_are_all_masked() {
        let text = "a=ghp_abcdefghijklmnop b=AKIAIOSFODNN7EXAMPLE";
        let out = redacted(text);
        assert!(!out.contains("ghp_"));
        assert!(!out.contains("AKIA"));
        assert_eq!(out.matches(MASK).count(), 2);
    }

    #[test]
    fn a_clean_value_round_trips_unchanged() {
        let text = "The memory bank stores dated facts.";
        assert_eq!(redacted(text), text);
        assert!(scan_secret_like_material(text).is_empty());
    }

    #[test]
    fn classes_are_reported_once_each_in_position_order() {
        let text = "ghp_abcdefghijklmnop then AKIAIOSFODNN7EXAMPLE";
        assert_eq!(
            secret_classes(text),
            [SecretClass::VendorToken, SecretClass::AwsAccessKey]
        );
    }

    #[test]
    fn a_key_at_the_very_start_is_found() {
        assert!(contains_secret_like_material("AKIAIOSFODNN7EXAMPLE"));
        assert!(contains_secret_like_material("sk-abc123def456"));
    }

    #[test]
    fn a_longer_value_keeps_its_trailing_delimiter() {
        let text = "password: \"hunter2\"";
        assert_eq!(redacted(text), "***");
    }
}
