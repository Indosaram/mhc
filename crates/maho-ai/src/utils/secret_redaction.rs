//! Port of senpi packages/ai/src/utils/secret-redaction.ts.
//!
//! Credential redaction for provider and gateway text, read token by token in one pass
//! (senpi#4283). The text is read ONCE, left to right, as alternating runs: a token (value
//! characters) and a delimiter (everything between two tokens), see `secret_lexer`. A token is
//! classified only from its own characters and the delimiters on either side of it
//! (`secret_classifier`); nothing inside the loop searches, slices from the start or re-reads
//! earlier text, so the work is linear in the input whatever its shape. The scanner uses no
//! regular expressions. A replaced range is a whole token (plus the base64 padding after it), the
//! rest of a token from where a credential shape starts, the whole value of a strict name (to its
//! closing quote, or to the end of a bare value; the scan continues after it), or a URL userinfo
//! password, from its ":" to the "@".

use super::secret_classifier::{
    COOKIE_ATTRIBUTES, COOKIE_WORDS, SCHEMES, STRICT_WORDS, credential_shape_end,
    credential_shape_start, is_inert, lower_token, name_start, secret_word,
};
use super::secret_lexer::{
    Code, bare_value_end, empty_delimiter, quoted_value_end, read_delimiter, read_unit,
    token_unit_length,
};

const MIN_SECRET_LENGTH: usize = 16;
const MIN_BEARER_LENGTH: usize = 8;
const OBJECT_SPAN: usize = 4096;
const MAX_COOKIE_PAIRS: usize = 64;
const MAX_USERINFO_LENGTH: usize = 256;

pub const DEFAULT_REPLACEMENT: &str = "[REDACTED]";

/// Append [start, end) to the sorted, disjoint ranges, merging what it overlaps.
fn add_range(ranges: &mut Vec<usize>, start: usize, end: usize) {
    let mut stop = end;
    while ranges.len() >= 2 && ranges[ranges.len() - 2] >= start {
        stop = stop.max(ranges[ranges.len() - 1]);
        ranges.truncate(ranges.len() - 2);
    }
    if let Some(last) = ranges.last_mut()
        && *last >= start
    {
        *last = (*last).max(stop);
    } else {
        ranges.push(start);
        ranges.push(stop);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    None,
    Scheme,
    Value,
    Cookie,
    CookieValue,
    Object,
}

fn in_set(set: &[&str], word: Option<&str>) -> bool {
    word.is_some_and(|word| set.contains(&word))
}

/// The [start, end) ranges of every credential in text, found in one left-to-right pass.
fn secret_ranges(text: &[u16], shapes_only: bool) -> Vec<usize> {
    let mut ranges = Vec::new();
    let mut before = empty_delimiter();
    let mut after = empty_delimiter();
    read_delimiter(text, 0, &mut before);
    if let Some(refusal) = before.refusal_start {
        return vec![refusal, text.len()];
    }
    let mut i = before.end;
    let mut mode = Mode::None;
    let mut strict = false;
    let mut quote: Code = -1;
    let mut quote_depth = 0usize;
    let mut cookie_pairs = 0usize;
    let mut cookie_attribute = false;
    let mut object_depth: i64 = 0;
    let mut object_end = 0usize;
    let mut bearer_armed = false;
    let mut userinfo_start: Option<usize> = None;
    while i < text.len() {
        let start = i;
        let mut slashes = 0usize;
        while i < text.len() {
            let length = token_unit_length(text, i);
            if length == 0 {
                break;
            }
            if read_unit(text, i).character == 47 {
                slashes += 1;
            }
            i += length;
        }
        let end = i;
        read_delimiter(text, end, &mut after);
        i = after.end;
        let value_end = after.padding_end;
        let value_length = value_end - start;
        if after.refusal_start.is_some() {
            // Do not let a bounded decoder refusal expose either an earlier shape or an unknown
            // named value.
            add_range(&mut ranges, start, text.len());
            break;
        }

        if !shapes_only && let Some(userinfo) = userinfo_start {
            if slashes > 0 || end - userinfo > MAX_USERINFO_LENGTH {
                userinfo_start = None;
            } else if let Some(at) = after.at {
                if at > userinfo {
                    add_range(&mut ranges, userinfo, at);
                }
                userinfo_start = None;
            } else if after.ends_userinfo {
                userinfo_start = None;
            }
        }
        let first = read_unit(text, start);
        let authority = slashes == 2
            && first.character == 47
            && read_unit(text, start + first.length).character == 47;
        if !shapes_only && authority && before.colon_only && after.colon_only {
            userinfo_start = Some(after.end);
        }

        if let Some(shape) = credential_shape_start(text, start, end, false) {
            // A second prefix can begin inside the first alphabet, not just after its end. Keep the
            // old conservative token span for joined credentials; only an unambiguous single key
            // releases its filename suffix.
            let joined =
                credential_shape_start(text, shape + read_unit(text, shape).length, end, true);
            let stop = if joined.is_some() {
                end
            } else {
                credential_shape_end(text, shape, end)
            };
            add_range(&mut ranges, shape, stop);
        }
        if shapes_only {
            continue;
        }
        if bearer_armed && value_length >= MIN_BEARER_LENGTH && !is_inert(text, start, end) {
            add_range(&mut ranges, start, value_end);
        }
        bearer_armed =
            after.space_only && lower_token(text, start, end, 6).as_deref() == Some("bearer");

        let idle = mode == Mode::None;
        if mode == Mode::Scheme && in_set(SCHEMES, lower_token(text, start, end, 6).as_deref()) {
            // An auth scheme is never the value itself: the token after it is, or there is none
            // ("Bearer [REDACTED]").
            mode = if after.space_only {
                Mode::Value
            } else {
                Mode::None
            };
        } else if (mode == Mode::Scheme || mode == Mode::Value) && strict {
            if !is_inert(text, start, end) {
                // The whole value of a strict name, read forward to where the field ends; the scan
                // resumes there.
                let stop = if quote >= 0 {
                    quoted_value_end(text, end, quote, quote_depth)
                } else {
                    bare_value_end(text, end)
                };
                add_range(&mut ranges, start, stop.max(value_end));
                if stop > end {
                    read_delimiter(text, stop, &mut after);
                    i = after.end;
                    bearer_armed = false;
                }
            }
            mode = Mode::None;
        } else if mode == Mode::Scheme || mode == Mode::Value {
            if value_length >= MIN_SECRET_LENGTH && !is_inert(text, start, end) {
                add_range(&mut ranges, start, value_end);
            }
            mode = Mode::None;
        } else if mode == Mode::Cookie {
            if after.pair_equals {
                cookie_attribute = in_set(
                    COOKIE_ATTRIBUTES,
                    lower_token(text, start, end, 11).as_deref(),
                );
                mode = Mode::CookieValue;
            } else {
                if value_length >= MIN_SECRET_LENGTH && !is_inert(text, start, end) {
                    add_range(&mut ranges, start, value_end);
                }
                mode = next_cookie_mode(after.cookie_gap, &mut cookie_pairs);
            }
        } else if mode == Mode::CookieValue {
            if !cookie_attribute && !is_inert(text, start, end) {
                add_range(&mut ranges, start, value_end);
            }
            mode = next_cookie_mode(after.cookie_gap, &mut cookie_pairs);
        } else if mode == Mode::Object {
            let value = before.separator && !before.opens_object;
            if value && value_length >= MIN_SECRET_LENGTH && !is_inert(text, start, end) {
                add_range(&mut ranges, start, value_end);
            }
            object_depth += after.brace_depth;
            if object_depth <= 0 || end > object_end {
                mode = Mode::None;
            }
        }

        if idle && (after.separator || (before.ends_with_lt && after.closes_tag)) {
            let word = secret_word(text, name_start(text, start, end), end);
            if in_set(COOKIE_WORDS, word) {
                mode = Mode::Cookie;
                cookie_pairs = 0;
            } else if word.is_some() && after.opens_object {
                mode = Mode::Object;
                object_depth = after.brace_depth;
                object_end = end + OBJECT_SPAN;
            } else if word.is_some() {
                mode = Mode::Scheme;
                strict = in_set(STRICT_WORDS, word);
                quote = after.quote;
                quote_depth = after.quote_backslashes;
            }
        }
        std::mem::swap(&mut before, &mut after);
    }
    ranges
}

fn next_cookie_mode(cookie_gap: bool, cookie_pairs: &mut usize) -> Mode {
    if cookie_gap {
        *cookie_pairs += 1;
        if *cookie_pairs < MAX_COOKIE_PAIRS {
            return Mode::Cookie;
        }
    }
    Mode::None
}

fn replace_ranges(text: &str, units: &[u16], ranges: &[usize], replacement: &str) -> String {
    if ranges.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for pair in ranges.chunks_exact(2) {
        out.push_str(&String::from_utf16_lossy(&units[copied..pair[0]]));
        out.push_str(replacement);
        copied = pair[1];
    }
    out.push_str(&String::from_utf16_lossy(&units[copied..]));
    out
}

/// Mask credentials in provider or gateway text by value, not by a list of names: the value after
/// a name that ends with a credential word (in a header, JSON, array, Go map, Python repr, XML,
/// URL query, form body, cookie or environment assignment, with ASCII, typographic or
/// HTML-escaped quotes, at any escaping depth; 16 characters or more, or the whole value at any
/// length under a password, secret, authorization or bearer name), every cookie value, every long
/// value of an object held under such a name, URL userinfo passwords, bearer tokens, and known key
/// prefixes and JWTs wherever a word starts with one (inside URLs, userinfo and paths too). Model
/// ids, error codes, request ids and names that merely contain a credential word stay readable.
/// One linear pass, no regular expressions.
pub fn redact_secrets(text: &str) -> String {
    redact_secrets_with(text, DEFAULT_REPLACEMENT)
}

pub fn redact_secrets_with(text: &str, replacement: &str) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    replace_ranges(text, &units, &secret_ranges(&units, false), replacement)
}

/// Mask only credential-shaped runs (known key prefixes and JWTs), whatever surrounds them. One
/// linear pass.
pub fn redact_credential_shapes(text: &str) -> String {
    redact_credential_shapes_with(text, DEFAULT_REPLACEMENT)
}

pub fn redact_credential_shapes_with(text: &str, replacement: &str) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    replace_ranges(text, &units, &secret_ranges(&units, true), replacement)
}
