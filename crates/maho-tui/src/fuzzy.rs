//! Fuzzy matching (port of senpi `fuzzy.ts`): all query characters must appear in order.
//! Lower score = better match. Indices are UTF-16 code units, as in senpi.

use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FuzzyMatch {
    pub matches: bool,
    pub score: f64,
}

const ALPHANUMERIC_SWAP_PENALTY: f64 = 5.0;

fn is_ascii_letter(unit: Option<&u16>) -> bool {
    unit.is_some_and(|&c| (97..=122).contains(&c))
}

fn is_ascii_digit(unit: Option<&u16>) -> bool {
    unit.is_some_and(|&c| (48..=57).contains(&c))
}

fn is_word_boundary_prefix(unit: u16) -> bool {
    u8::try_from(unit).is_ok_and(|b| {
        matches!(
            b,
            b' ' | b'\t' | b'\n' | b'\r' | b'-' | b'_' | b'.' | b'/' | b':'
        )
    })
}

fn score_match(query: &[u16], text: &[u16]) -> FuzzyMatch {
    if query.is_empty() {
        return FuzzyMatch {
            matches: true,
            score: 0.0,
        };
    }
    if query.len() > text.len() {
        return FuzzyMatch {
            matches: false,
            score: 0.0,
        };
    }

    let mut query_index = 0;
    let mut score = 0.0;
    let mut last_match_index: isize = -1;
    let mut consecutive_matches = 0.0;

    let mut i = 0;
    while i < text.len() && query_index < query.len() {
        if text[i] == query[query_index] {
            let is_word_boundary = i == 0 || is_word_boundary_prefix(text[i - 1]);
            let idx = isize::try_from(i).unwrap_or(isize::MAX);
            if last_match_index == idx - 1 {
                consecutive_matches += 1.0;
                score -= consecutive_matches * 5.0;
            } else {
                consecutive_matches = 0.0;
                if last_match_index >= 0 {
                    score += (idx - last_match_index - 1) as f64 * 2.0;
                }
            }
            if is_word_boundary {
                score -= 10.0;
            }
            score += i as f64 * 0.1;
            last_match_index = idx;
            query_index += 1;
        }
        i += 1;
    }

    if query_index < query.len() {
        return FuzzyMatch {
            matches: false,
            score: 0.0,
        };
    }
    if query == text {
        score -= 100.0;
    }
    FuzzyMatch {
        matches: true,
        score,
    }
}

/// Insertion-ordered set, like a JS `Set` iterated with spread.
struct OrderedSet {
    seen: HashSet<Vec<u16>>,
    items: Vec<Vec<u16>>,
}

impl OrderedSet {
    fn new() -> Self {
        Self {
            seen: HashSet::new(),
            items: Vec::new(),
        }
    }

    fn add(&mut self, value: Vec<u16>) {
        if self.seen.insert(value.clone()) {
            self.items.push(value);
        }
    }
}

fn rotated(query: &[u16], split: usize) -> Vec<u16> {
    let mut v = query[split..].to_vec();
    v.extend_from_slice(&query[..split]);
    v
}

fn add_whole_token_swap(variants: &mut OrderedSet, query: &[u16]) {
    let mut split = 0;
    while split < query.len() && is_ascii_letter(query.get(split)) {
        split += 1;
    }
    if split > 0 && split < query.len() {
        let mut digit_end = split;
        while digit_end < query.len() && is_ascii_digit(query.get(digit_end)) {
            digit_end += 1;
        }
        if digit_end == query.len() {
            variants.add(rotated(query, split));
        }
    }

    split = 0;
    while split < query.len() && is_ascii_digit(query.get(split)) {
        split += 1;
    }
    if split > 0 && split < query.len() {
        let mut letter_end = split;
        while letter_end < query.len() && is_ascii_letter(query.get(letter_end)) {
            letter_end += 1;
        }
        if letter_end == query.len() {
            variants.add(rotated(query, split));
        }
    }
}

fn build_alphanumeric_swap_queries(query: &[u16]) -> Vec<Vec<u16>> {
    let mut variants = OrderedSet::new();
    add_whole_token_swap(&mut variants, query);
    for i in 0..query.len().saturating_sub(1) {
        let current = query.get(i);
        let next = query.get(i + 1);
        let swap = (is_ascii_letter(current) && is_ascii_digit(next))
            || (is_ascii_digit(current) && is_ascii_letter(next));
        if !swap {
            continue;
        }
        let mut v = query[..i].to_vec();
        v.push(query[i + 1]);
        v.push(query[i]);
        v.extend_from_slice(&query[i + 2..]);
        variants.add(v);
    }
    variants.items
}

fn lower_units(s: &str) -> Vec<u16> {
    s.to_lowercase().encode_utf16().collect()
}

pub fn fuzzy_match(query: &str, text: &str) -> FuzzyMatch {
    let query_lower = lower_units(query);
    let text_lower = lower_units(text);

    let direct = score_match(&query_lower, &text_lower);
    if direct.matches {
        return direct;
    }

    let mut best_swap: Option<FuzzyMatch> = None;
    for variant in build_alphanumeric_swap_queries(&query_lower) {
        let m = score_match(&variant, &text_lower);
        if !m.matches {
            continue;
        }
        let score = m.score + ALPHANUMERIC_SWAP_PENALTY;
        if best_swap.is_none_or(|b| score < b.score) {
            best_swap = Some(FuzzyMatch {
                matches: true,
                score,
            });
        }
    }
    best_swap.unwrap_or(direct)
}

fn is_js_whitespace(c: char) -> bool {
    crate::utils::is_whitespace_char(c.encode_utf8(&mut [0; 4]))
}

/// Filter and sort items by fuzzy match quality (best first). Whitespace- and slash-separated
/// tokens must all match.
pub fn fuzzy_filter<T: Clone>(items: &[T], query: &str, get_text: impl Fn(&T) -> String) -> Vec<T> {
    let trimmed = query.trim_matches(is_js_whitespace);
    if trimmed.is_empty() {
        return items.to_vec();
    }
    let tokens: Vec<&str> = trimmed
        .split(|c: char| c == '/' || is_js_whitespace(c))
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return items.to_vec();
    }

    let mut results: Vec<(usize, f64)> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let text = get_text(item);
        let mut total = 0.0;
        let mut all_match = true;
        for token in &tokens {
            let m = fuzzy_match(token, &text);
            if m.matches {
                total += m.score;
            } else {
                all_match = false;
                break;
            }
        }
        if all_match {
            results.push((index, total));
        }
    }
    results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    results.into_iter().map(|(i, _)| items[i].clone()).collect()
}

#[cfg(test)]
#[path = "fuzzy_tests.rs"]
mod tests;
