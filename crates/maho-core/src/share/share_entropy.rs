//! Port of senpi packages/coding-agent/src/core/share/share-entropy.ts.
//!
//! Generic secret detection for `/share`: values that match no known provider shape but look
//! machine-generated. Lowercase hex, UUIDs and ULIDs never qualify, identifiers and slash lists that
//! read as words are spared, and SRI integrity hashes stay. A token broken across a line, an escaped
//! newline, a backslash continuation or a string-literal seam is judged as one value.

use std::collections::HashMap;
use std::sync::OnceLock;

use fancy_regex::{Captures, Regex};

const RUN_CHAR: &str = "[A-Za-z0-9+/_-]";
const PAD: &str = "={0,2}";
/// Every way a value gets broken in a transcript: a newline, `\n` escaped inside JSON, a `\`
/// continuation, `" + "` or `", "`.
const GAP: &str =
    r#"(?:\\?\r?\n[ \t]*|(?:\\r)?\\n[ \t]*|["'][ \t]*[+,]?[ \t]*(?:\r?\n[ \t]*)?["'])"#;
const REDACTED: &str = "<redacted>";

struct Patterns {
    single_run: Regex,
    split_run: Regex,
    wrapped_tail: Regex,
    word_piece: Regex,
    case_piece: Regex,
    edge_start: Regex,
    edge_end: Regex,
}

impl Patterns {
    fn build() -> Result<Self, Box<fancy_regex::Error>> {
        let run = RUN_CHAR;
        let pad = PAD;
        let gap = GAP;
        Ok(Self {
            single_run: Regex::new(&format!("(?<!{run}){run}{{20,}}{pad}(?![A-Za-z0-9+/=_-])"))
                .map_err(Box::new)?,
            split_run: Regex::new(&format!(
                "(?<!{run})({run}{{8,}})({gap})({run}{{8,}}{pad})(?![A-Za-z0-9+/=_-])"
            ))
            .map_err(Box::new)?,
            wrapped_tail: Regex::new(&format!(
                "(<redacted>)({gap})({run}{{6,}}{pad})(?![A-Za-z0-9+/=_-])"
            ))
            .map_err(Box::new)?,
            word_piece: Regex::new("[A-Z]?[a-z]{3,}|[0-9]+").map_err(Box::new)?,
            case_piece: Regex::new("[A-Z]?[a-z]+|[A-Z]+(?![a-z])|[0-9]+").map_err(Box::new)?,
            edge_start: Regex::new(&format!("^({run}{{8,}}{pad})")).map_err(Box::new)?,
            edge_end: Regex::new(&format!("(?<!{run})({run}{{8,}})$")).map_err(Box::new)?,
        })
    }
}

fn patterns() -> Option<&'static Patterns> {
    static PATTERNS: OnceLock<Option<Patterns>> = OnceLock::new();
    PATTERNS.get_or_init(|| Patterns::build().ok()).as_ref()
}

/// Shannon entropy in bits per character.
pub fn shannon_bits_per_char(text: &str) -> f64 {
    let mut counts: HashMap<char, usize> = HashMap::new();
    let mut total = 0usize;
    for ch in text.chars() {
        *counts.entry(ch).or_insert(0) += 1;
        total += 1;
    }
    let total = total as f64;
    let mut bits = 0.0;
    for n in counts.values() {
        let p = *n as f64 / total;
        bits -= p * p.log2();
    }
    bits
}

fn is_plain_lower(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '@' | '-')
        })
}

fn is_constant_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn is_vowel_y(c: char) -> bool {
    matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u' | 'y')
}

fn pronounceable(piece: &str) -> bool {
    if !piece.is_empty() && piece.chars().all(|c| c.is_ascii_digit()) {
        return piece.chars().count() <= 3;
    }
    if !piece.chars().any(is_vowel_y) {
        return false;
    }
    let mut streak = 0;
    for c in piece.chars() {
        if is_vowel_y(c) {
            streak = 0;
        } else {
            streak += 1;
            if streak >= 4 {
                return false;
            }
        }
    }
    true
}

/// Identifiers are made of words: most of the run is long pronounceable lowercase pieces and the
/// letters carry English vowel density (random letters run at about 19%).
pub fn reads_as_words(run: &str, coverage: f64) -> bool {
    let Some(p) = patterns() else { return false };
    let letters: Vec<char> = run.chars().filter(char::is_ascii_alphabetic).collect();
    let vowels = letters
        .iter()
        .filter(|c| matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u'))
        .count();
    if letters.is_empty() || (vowels as f64) / (letters.len() as f64) < 0.3 {
        return false;
    }
    let run_len = run.chars().count();
    let covered: usize = p
        .word_piece
        .find_iter(run)
        .filter_map(Result::ok)
        .map(|m| m.as_str())
        .filter(|piece| pronounceable(piece))
        .map(|piece| piece.chars().count())
        .sum();
    let pieces = p.case_piece.find_iter(run).filter_map(Result::ok).count();
    (covered as f64) >= (run_len as f64) * coverage
        && pieces > 0
        && (run_len as f64) / (pieces as f64) >= 3.0
}

/// A path or a slash list (`a/b/c.ts`, `1/true/yes`, `VISUAL/EDITOR`).
fn reads_as_path(run: &str) -> bool {
    let segments: Vec<&str> = run.split('/').filter(|s| !s.is_empty()).collect();
    segments.len() >= 2 && segments.iter().all(|s| is_plain_segment(s))
}

fn is_plain_segment(segment: &str) -> bool {
    is_plain_lower(segment) || is_constant_name(segment) || reads_as_words(segment, 0.6)
}

/// Mixed case, high per-character entropy for its length, and not words or a path.
pub fn looks_generated(run: &str) -> bool {
    let body = run.trim_end_matches('=');
    let len = body.chars().count();
    if len < 16 {
        return false;
    }
    let upper = body.chars().filter(char::is_ascii_uppercase).count();
    let lower = body.chars().filter(char::is_ascii_lowercase).count();
    let digits = body.chars().filter(char::is_ascii_digit).count();
    let symbols = body.chars().filter(|c| matches!(c, '+' | '/')).count();
    let classes = [upper, lower, digits, symbols]
        .iter()
        .filter(|n| **n > 0)
        .count();
    if lower < 2 || (upper < 2 && classes < 3) {
        return false;
    }
    if shannon_bits_per_char(body) < f64::min(3.8, (len as f64).log2() - 1.2) {
        return false;
    }
    !reads_as_words(body, 0.6) && !reads_as_path(body)
}

/// A split is joined only when neither half is ordinary text on its own.
fn plain_half(half: &str) -> bool {
    is_plain_lower(half)
        || is_constant_name(half)
        || reads_as_path(half)
        || reads_as_words(half, 0.85)
}

fn integrity_at(run: &str, offset: usize, whole: &str) -> bool {
    let run_is_integrity = ["sha256-", "sha384-", "sha512-"]
        .iter()
        .any(|prefix| run.starts_with(prefix));
    if run_is_integrity {
        return true;
    }
    let before = whole.get(..offset).unwrap_or("");
    ["sha256-", "sha384-", "sha512-"]
        .iter()
        .any(|prefix| before.ends_with(prefix))
}

/// Whether `head + tail`, joined across a gap, is one generated value.
pub fn joined_looks_generated(head: &str, tail: &str) -> bool {
    !plain_half(head) && !plain_half(tail) && looks_generated(&format!("{head}{tail}"))
}

/// Replaces generated-looking values with `<redacted>`, calling `on_redact` once per replacement.
pub fn redact_generated_values(text: &str, on_redact: &mut dyn FnMut()) -> String {
    let Some(p) = patterns() else {
        return text.to_owned();
    };
    let mut result = p
        .split_run
        .replace_all(text, |caps: &Captures| {
            let whole_match = caps.get(0).map_or("", |m| m.as_str());
            let offset = caps.get(0).map_or(0, |m| m.start());
            let head = caps.get(1).map_or("", |m| m.as_str());
            let gap = caps.get(2).map_or("", |m| m.as_str());
            let tail = caps.get(3).map_or("", |m| m.as_str());
            if integrity_at(head, offset, text) || !joined_looks_generated(head, tail) {
                return whole_match.to_owned();
            }
            on_redact();
            format!("{REDACTED}{gap}{REDACTED}")
        })
        .into_owned();
    let whole = result.clone();
    result = p
        .single_run
        .replace_all(&whole, |caps: &Captures| {
            let run = caps.get(0).map_or("", |m| m.as_str());
            let offset = caps.get(0).map_or(0, |m| m.start());
            if integrity_at(run, offset, &whole) {
                return run.to_owned();
            }
            if run.chars().count() < 32
                && !(run.contains('+') || run.contains('/') || run.ends_with('='))
            {
                return run.to_owned();
            }
            if !looks_generated(run) {
                return run.to_owned();
            }
            on_redact();
            REDACTED.to_owned()
        })
        .into_owned();
    p.wrapped_tail
        .replace_all(&result, |caps: &Captures| {
            let whole_match = caps.get(0).map_or("", |m| m.as_str());
            let redacted = caps.get(1).map_or("", |m| m.as_str());
            let gap = caps.get(2).map_or("", |m| m.as_str());
            let tail = caps.get(3).map_or("", |m| m.as_str());
            let has_alpha = tail.chars().any(|c| c.is_ascii_alphabetic());
            let has_digit = tail.chars().any(|c| c.is_ascii_digit());
            let has_upper = tail.chars().any(|c| c.is_ascii_uppercase());
            let has_lower = tail.chars().any(|c| c.is_ascii_lowercase());
            let mixed = has_alpha && (has_digit || (has_upper && has_lower));
            if !mixed || is_plain_lower(tail) || is_constant_name(tail) || reads_as_path(tail) {
                return whole_match.to_owned();
            }
            on_redact();
            format!("{redacted}{gap}{REDACTED}")
        })
        .into_owned()
}

/// Two adjacent strings whose touching ends form one generated value lose both ends.
pub fn redact_across_boundary(
    left: &str,
    right: &str,
    on_redact: &mut dyn FnMut(),
) -> (String, String) {
    let unchanged = || (left.to_owned(), right.to_owned());
    let Some(p) = patterns() else {
        return unchanged();
    };
    let end = p
        .edge_end
        .captures(left)
        .ok()
        .flatten()
        .and_then(|c| c.get(1).map(|m| m.as_str().to_owned()));
    let start = p
        .edge_start
        .captures(right)
        .ok()
        .flatten()
        .and_then(|c| c.get(1).map(|m| m.as_str().to_owned()));
    let (Some(end), Some(start)) = (end, start) else {
        return unchanged();
    };
    if !joined_looks_generated(&end, &start) {
        return unchanged();
    }
    on_redact();
    (
        format!("{}{REDACTED}", &left[..left.len() - end.len()]),
        format!("{REDACTED}{}", &right[start.len()..]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "aB3xK9mQ2zP7vL4nR8tY1wE6uI5oA0sD";

    #[test]
    fn entropy_of_repeated_char_is_zero() {
        assert!(shannon_bits_per_char("aaaa").abs() < f64::EPSILON);
    }

    #[test]
    fn redacts_a_generated_run_and_counts_it() {
        let mut hits = 0;
        let out = redact_generated_values(&format!("token {SECRET} end"), &mut || hits += 1);
        assert_eq!(out, "token <redacted> end");
        assert_eq!(hits, 1);
    }

    #[test]
    fn spares_integrity_hashes_and_plain_words() {
        let mut hits = 0;
        let sri = format!("sha512-{SECRET}");
        assert_eq!(redact_generated_values(&sri, &mut || hits += 1), sri);
        let words = "a/long/path/to/some_module_name.ts";
        assert_eq!(redact_generated_values(words, &mut || hits += 1), words);
        assert_eq!(hits, 0);
    }

    #[test]
    fn joins_a_run_split_across_a_newline() {
        let mut hits = 0;
        let (head, tail) = SECRET.split_at(16);
        let out = redact_generated_values(&format!("{head}\n{tail}"), &mut || hits += 1);
        assert_eq!(out, "<redacted>\n<redacted>");
        assert_eq!(hits, 1);
    }

    #[test]
    fn redacts_across_a_boundary() {
        let mut hits = 0;
        let (head, tail) = SECRET.split_at(16);
        let (left, right) =
            redact_across_boundary(&format!("x {head}"), &format!("{tail} y"), &mut || {
                hits += 1
            });
        assert_eq!(
            (left.as_str(), right.as_str()),
            ("x <redacted>", "<redacted> y")
        );
        assert_eq!(hits, 1);
    }
}
