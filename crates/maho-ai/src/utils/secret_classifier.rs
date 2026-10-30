//! Port of senpi packages/ai/src/utils/secret-classifier.ts.
//!
//! What the credential redactor recognizes (senpi#4283): the credential words a name ends with,
//! and the shapes of known key prefixes and JWTs. Each check reads only the characters of the
//! token it is given, forward, without a regular expression.

use super::secret_lexer::{
    Code, code_at, is_alnum, is_base64_url, is_digit, is_lower, is_name_char, is_upper, matches_at,
    read_unit,
};

/// The last component of a credential name, after digits and qualifiers are stripped (see
/// `secret_word`).
const SECRET_WORDS: &[&str] = &[
    "key",
    "apikey",
    "accesskey",
    "secretkey",
    "privatekey",
    "token",
    "accesstoken",
    "refreshtoken",
    "authtoken",
    "apitoken",
    "idtoken",
    "secret",
    "clientsecret",
    "auth",
    "authorization",
    "bearer",
    "cookie",
    "cookies",
    "password",
    "passwd",
    "pwd",
    "passphrase",
    "credential",
    "credentials",
    "signature",
    "sig",
];
/// Names whose whole value is a credential at any length; every other credential name needs a
/// 16-character value.
pub const STRICT_WORDS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "passphrase",
    "secret",
    "clientsecret",
    "authorization",
    "bearer",
];
pub const COOKIE_WORDS: &[&str] = &["cookie", "cookies"];
/// A trailing component that names a part of the credential the name before it holds:
/// api_key_value, apiKeyId.
const QUALIFIERS: &[&str] = &["value", "val", "id"];
pub const SCHEMES: &[&str] = &["bearer", "basic", "token", "apikey"];
/// Values that are never credentials, including a replacement an earlier pass wrote (redaction is
/// idempotent).
const INERT_VALUES: &[&str] = &["redacted", "null", "true", "false", "none", "undefined"];
pub const COOKIE_ATTRIBUTES: &[&str] = &[
    "path",
    "domain",
    "expires",
    "max-age",
    "samesite",
    "secure",
    "httponly",
    "priority",
    "partitioned",
];

fn is_upper_or_digit(c: Code) -> bool {
    is_digit(c) || is_upper(c)
}
fn is_slack_char(c: Code) -> bool {
    is_alnum(c) || c == 45
}
fn is_pat_char(c: Code) -> bool {
    is_alnum(c) || c == 95
}
fn is_word_char(c: Code) -> bool {
    is_alnum(c) || c == 95
}

fn push_lower(current: &mut Vec<u16>, c: Code) {
    let lowered = if is_upper(c) { c + 32 } else { c };
    current.push(u16::try_from(lowered).unwrap_or_default());
}

/// Split a name into lowercase components on "_", "-", "." and camelCase, reading only the name's
/// own characters.
fn name_components(text: &[u16], start: usize, end: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current: Vec<u16> = Vec::new();
    for i in start..end {
        let c = code_at(text, i);
        if c == 95 || c == 45 || c == 46 {
            if !current.is_empty() {
                parts.push(String::from_utf16_lossy(&current));
            }
            current.clear();
            continue;
        }
        if is_upper(c) && !current.is_empty() {
            let previous = code_at(text, i - 1);
            let next = if i + 1 < end { code_at(text, i + 1) } else { 0 };
            if is_lower(previous) || is_digit(previous) || (is_upper(previous) && is_lower(next)) {
                parts.push(String::from_utf16_lossy(&current));
                current.clear();
            }
        }
        push_lower(&mut current, c);
    }
    if !current.is_empty() {
        parts.push(String::from_utf16_lossy(&current));
    }
    parts
}

fn part_at(parts: &[String], index: isize) -> &str {
    usize::try_from(index)
        .ok()
        .and_then(|index| parts.get(index))
        .map_or("", String::as_str)
}

fn is_all_digits(part: &str) -> bool {
    part.bytes().all(|byte| byte.is_ascii_digit())
}

fn without_trailing_digits(part: &str) -> &str {
    let bytes = part.as_bytes();
    let mut end = bytes.len();
    while end > 1 && bytes[end - 1].is_ascii_digit() {
        end -= 1;
    }
    &part[..end]
}

/// The credential word a name ends with, after its normalization: numbered slots
/// (OPENAI_API_KEY_2, token2, x-api-key-1) and a trailing qualifier (api_key_value, apiKeyId)
/// still end with the word. "session" counts only as the whole name, so session_id stays readable;
/// a word elsewhere in a name (token_type, primary_key_column) or inside a word (monkey, tokenizer,
/// max_tokens) does not count.
pub fn secret_word(text: &[u16], start: usize, end: usize) -> Option<&'static str> {
    let parts = name_components(text, start, end);
    if parts.len() == 1 && parts[0] == "session" {
        return Some("session");
    }
    let mut last = parts.len() as isize - 1;
    loop {
        while last >= 0 && is_all_digits(part_at(&parts, last)) {
            last -= 1;
        }
        if last > 0 && QUALIFIERS.contains(&part_at(&parts, last)) {
            last -= 1;
        } else {
            break;
        }
    }
    if last < 0 {
        return None;
    }
    let word = without_trailing_digits(part_at(&parts, last));
    SECRET_WORDS
        .iter()
        .copied()
        .find(|candidate| *candidate == word)
}

/// Where the name ends a token: its last run of name characters (a URL path or escape before it
/// is not the name).
pub fn name_start(text: &[u16], start: usize, end: usize) -> usize {
    let mut i = end;
    while i > start && is_name_char(code_at(text, i - 1)) {
        i -= 1;
    }
    i
}

pub fn lower_token(text: &[u16], start: usize, end: usize, max_length: usize) -> Option<String> {
    if end - start > max_length {
        return None;
    }
    Some(String::from_utf16_lossy(&text[start..end]).to_lowercase())
}

pub fn is_inert(text: &[u16], start: usize, end: usize) -> bool {
    lower_token(text, start, end, 9).is_some_and(|word| INERT_VALUES.contains(&word.as_str()))
}

struct PrefixRule {
    prefixes: &'static [&'static str],
    min: usize,
    chars: fn(Code) -> bool,
    /// The run after the prefix is exactly min characters long (AWS access key ids).
    exact: bool,
}

/// Credential-shaped tokens recognized by their prefix wherever a word starts with it.
const PREFIX_RULES: &[PrefixRule] = &[
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
        let mut run = 0;
        let limit = if rule.exact { rule.min + 1 } else { rule.min };
        let mut i = start + prefix.len();
        while i < end && run < limit && (rule.chars)(code_at(text, i)) {
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
pub fn credential_shape_end(text: &[u16], start: usize, end: usize) -> usize {
    for rule in PREFIX_RULES {
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
    // JWTs have their own segment grammar; retain the conservative span for those.
    end
}

/// Whether a JWT (three base64url segments joined by ".", the first a JSON header "eyJ") starts at
/// p: `None` when one does, else where the next JWT candidate may start. A candidate before the
/// first "." after p sees the same segments with a shorter first one, so it cannot match either;
/// skipping to that "." caps every walk at three segments, and each segment is walked by at most
/// three candidates.
fn jwt_at(text: &[u16], p: usize, end: usize) -> Option<usize> {
    if !matches_at(text, p, "eyJ", end) {
        return Some(p + 1);
    }
    let mut segments = 1;
    let mut length = 0;
    let mut first_dot: Option<usize> = None;
    let mut i = p;
    while i < end {
        let unit = read_unit(text, i);
        let c = unit.character;
        if c == 46 {
            if first_dot.is_none() {
                first_dot = Some(i);
            }
            let next = if i + unit.length < end {
                read_unit(text, i + unit.length).character
            } else {
                0
            };
            if segments == 3 {
                return if length >= 5 && !is_base64_url(next) {
                    None
                } else {
                    first_dot
                };
            }
            if length < (if segments == 1 { 8 } else { 5 }) {
                return first_dot;
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

/// Where the first credential-shaped run in [start, end) begins: a known key prefix or a JWT at
/// the start of a word anywhere in the token (after "/", "+", "~", "-", "." or "%", so URL
/// userinfo, paths and joined values count), including percent-encoded token punctuation, never
/// inside a word ("tasks-..." is not "sk-..."). After a credential has already been found,
/// `within_credential` also checks inside words: an adjoining key may have had its prefix consumed
/// by the first key's alphabet. The caller retains the whole token in that ambiguous case.
/// Constant work per character plus the JWT walks bounded above.
pub fn credential_shape_start(
    text: &[u16],
    start: usize,
    end: usize,
    within_credential: bool,
) -> Option<usize> {
    let mut stop = end;
    while stop > start && code_at(text, stop - 1) == 46 {
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
