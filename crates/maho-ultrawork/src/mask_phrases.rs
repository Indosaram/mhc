//! Masking of relayed headers and prohibitive phrasing.

use crate::mask::{Span, lines};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    From,
    To,
}

fn header_side(line: &str) -> Option<Side> {
    let lower = line.trim_start().to_ascii_lowercase();
    if lower.starts_with("from:") || lower.starts_with("sender:") {
        Some(Side::From)
    } else if lower.starts_with("to:") || lower.starts_with("recipient:") {
        Some(Side::To)
    } else {
        None
    }
}

fn arrow_header_end(line: &str) -> Option<usize> {
    let colon = line.find(':')?;
    let prefix = &line[..colon];
    let arrowed = prefix.contains("->") || prefix.contains('\u{2192}');
    let plain = prefix
        .chars()
        .all(|c| c.is_alphanumeric() || " .@_-[]>\u{2192}".contains(c));
    (arrowed && plain && prefix.len() <= 100).then_some(colon + 1)
}

/// A relayed message opening with an all-caps sender tag (`[REPORT] ...`) is someone else's text.
fn relay_tag(text: &str) -> bool {
    let Some(rest) = text.trim_start().strip_prefix('[') else {
        return false;
    };
    let name = rest
        .bytes()
        .take_while(|&b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        .count();
    rest.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        && name >= 3
        && rest.as_bytes().get(name) == Some(&b']')
}

pub(crate) fn header_spans(text: &str) -> Vec<Span> {
    if relay_tag(text) {
        return vec![(0, text.len())];
    }
    let all = lines(text);
    let mut spans = Vec::new();
    for (n, &(start, end)) in all.iter().enumerate() {
        if let Some(offset) = arrow_header_end(&text[start..end]) {
            spans.push((start, start + offset));
        }
        let Some(side) = header_side(&text[start..end]) else {
            continue;
        };
        let partner = all
            .iter()
            .enumerate()
            .skip(n + 1)
            .take(2)
            .find(|(_, s)| header_side(&text[s.0..s.1]).is_some_and(|other| other != side));
        if let Some((_, &(_, partner_end))) = partner {
            spans.push((start, partner_end));
        }
    }
    spans
}

pub(crate) const NEGATIONS: [&str; 12] = [
    "never", "not", "dont", "don't", "doesnt", "doesn't", "wont", "won't", "cant", "can't",
    "cannot", "avoid",
];
const INVOCATIONS: [&str; 19] = [
    "load",
    "loading",
    "arm",
    "arming",
    "use",
    "using",
    "enable",
    "enabling",
    "run",
    "running",
    "trigger",
    "triggering",
    "invoke",
    "activate",
    "apply",
    "start",
    "launch",
    "engage",
    "turn",
];
const CONTRASTS: [&str; 6] = ["but", "however", "instead", "though", "although", "yet"];

fn words(text: &str) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let mut current: Option<usize> = None;
    for (index, ch) in text.char_indices() {
        let in_word = ch.is_alphanumeric() || ch == '\'' || ch == '\u{2019}';
        match (in_word, current) {
            (true, None) => current = Some(index),
            (false, Some(start)) => {
                out.push((start, index, normalise(&text[start..index])));
                current = None;
            }
            _ => {}
        }
    }
    if let Some(start) = current {
        out.push((start, text.len(), normalise(&text[start..])));
    }
    out
}

fn normalise(word: &str) -> String {
    word.to_lowercase().replace('\u{2019}', "'")
}

fn clause_end(text: &str, from: usize, tokens: &[(usize, usize, String)]) -> usize {
    let bytes = text.as_bytes();
    let contrast = tokens
        .iter()
        .find(|(start, _, word)| *start >= from && CONTRASTS.contains(&word.as_str()))
        .map_or(bytes.len(), |(start, _, _)| *start);
    let sentence = (from..bytes.len().min(contrast)).find(|&index| {
        matches!(bytes[index], b';' | b'\n')
            || (matches!(bytes[index], b'.' | b'!' | b'?')
                && bytes.get(index + 1).is_none_or(u8::is_ascii_whitespace))
    });
    sentence.unwrap_or(contrast)
}

pub(crate) fn prohibitive_spans(text: &str) -> Vec<Span> {
    let tokens = words(text);
    let mut spans = Vec::new();
    for (n, (start, negation_end, word)) in tokens.iter().enumerate() {
        if !NEGATIONS.contains(&word.as_str()) {
            continue;
        }
        let verb = tokens
            .iter()
            .skip(n + 1)
            .take(3)
            .find(|(_, _, other)| INVOCATIONS.contains(&other.as_str()));
        if let Some((verb_start, verb_end, _)) = verb
            && !text[*negation_end..*verb_start].contains([',', ';', '.', '!', '?', '\n'])
        {
            spans.push((*start, clause_end(text, *verb_end, &tokens)));
        }
    }
    spans
}
