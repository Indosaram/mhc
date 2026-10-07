//! Recall BM25 ranking with CJK bigram tokenization (pin `recall/bm25.ts`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_normalization::UnicodeNormalization;

use super::english_stem::stem_english_token;
use super::provider::RecallDocument;

const K1: f64 = 1.5;
const B: f64 = 0.75;

fn is_hangul(ch: char) -> bool {
    matches!(ch as u32,
        0x1100..=0x11FF | 0x3130..=0x318F | 0xA960..=0xA97F | 0xAC00..=0xD7A3 | 0xD7B0..=0xD7FF)
}

fn is_han(ch: char) -> bool {
    matches!(ch as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F)
}

fn is_kana(ch: char) -> bool {
    matches!(ch as u32, 0x3040..=0x309F | 0x30A0..=0x30FF | 0x31F0..=0x31FF)
}

fn is_prolonged_sound_mark(ch: char) -> bool {
    ch == '\u{30fc}'
}

/// True when the character is part of the CJK class the pinned tokenizer groups into runs.
pub fn is_cjk_char(ch: char) -> bool {
    is_hangul(ch) || is_han(ch) || is_kana(ch) || is_prolonged_sound_mark(ch)
}

pub fn is_han_character(token: &str) -> bool {
    let mut chars = token.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) => is_han(ch),
        _ => false,
    }
}

fn is_word_char(ch: char) -> bool {
    matches!(
        get_general_category(ch),
        GeneralCategory::LowercaseLetter
            | GeneralCategory::UppercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
            | GeneralCategory::NonspacingMark
            | GeneralCategory::SpacingMark
            | GeneralCategory::EnclosingMark
            | GeneralCategory::DecimalNumber
            | GeneralCategory::LetterNumber
            | GeneralCategory::OtherNumber
    )
}

struct RecallRun {
    pieces: Vec<String>,
    han_characters: Vec<String>,
}

fn split_run(token: &str) -> RecallRun {
    let characters: Vec<char> = token.chars().collect();
    let mut pieces = vec![token.to_string()];
    if characters.len() < 2 || !characters.iter().all(|ch| is_cjk_char(*ch)) {
        return RecallRun {
            pieces,
            han_characters: Vec::new(),
        };
    }
    if characters.len() > 2 {
        for pair in characters.windows(2) {
            pieces.push(pair.iter().collect());
        }
    }
    RecallRun {
        pieces,
        han_characters: characters
            .into_iter()
            .filter(|ch| is_han(*ch))
            .map(String::from)
            .collect(),
    }
}

fn runs(text: &str) -> Vec<RecallRun> {
    let normalized: String = text.nfkc().collect::<String>().to_lowercase();
    let mut runs = Vec::new();
    let mut current = String::new();
    let mut current_is_cjk = false;
    for ch in normalized.chars() {
        let cjk = is_cjk_char(ch);
        if cjk || is_word_char(ch) {
            if !current.is_empty() && cjk != current_is_cjk {
                runs.push(split_run(&current));
                current.clear();
            }
            current.push(ch);
            current_is_cjk = cjk;
            continue;
        }
        if !current.is_empty() {
            runs.push(split_run(&current));
            current.clear();
        }
    }
    if !current.is_empty() {
        runs.push(split_run(&current));
    }
    runs
}

struct RecallTokens {
    tokens: Vec<String>,
    standalone_han_characters: usize,
}

fn tokenize(text: &str) -> RecallTokens {
    let mut tokens = Vec::new();
    let mut standalone_han_characters = 0usize;
    for run in runs(text) {
        for piece in run.pieces {
            tokens.push(piece);
        }
        standalone_han_characters += run.han_characters.len();
        for character in run.han_characters {
            tokens.push(character);
        }
    }
    RecallTokens {
        tokens,
        standalone_han_characters,
    }
}

/// NFKC-normalized, lowercased tokens with CJK runs expanded into bigrams.
pub fn tokenize_recall_text(text: &str) -> Vec<String> {
    tokenize(text).tokens
}

/// Index and query terms: the tokens with English suffixes folded.
pub fn recall_terms(text: &str) -> Vec<String> {
    tokenize_recall_text(text)
        .into_iter()
        .map(|token| stem_english_token(&token))
        .collect()
}

#[derive(Default)]
struct RecallBm25Index {
    term_frequencies: Vec<BTreeMap<String, u64>>,
    lengths: Vec<usize>,
    document_frequency: BTreeMap<String, u64>,
    average_length: f64,
}

fn build_index(documents: &[RecallDocument]) -> RecallBm25Index {
    let mut index = RecallBm25Index::default();
    for document in documents {
        let tokens = tokenize(&format!("{}\n{}", document.description, document.body));
        let mut frequencies: BTreeMap<String, u64> = BTreeMap::new();
        for token in tokens.tokens.iter().map(|token| stem_english_token(token)) {
            *frequencies.entry(token).or_insert(0) += 1;
        }
        for token in frequencies.keys() {
            *index.document_frequency.entry(token.clone()).or_insert(0) += 1;
        }
        index
            .lengths
            .push(tokens.tokens.len() - tokens.standalone_han_characters);
        index.term_frequencies.push(frequencies);
    }
    let total: usize = index.lengths.iter().sum();
    index.average_length = if documents.is_empty() {
        0.0
    } else {
        total as f64 / documents.len() as f64
    };
    index
}

type RecallBm25IndexCache = Mutex<Option<Vec<(usize, usize, std::sync::Arc<RecallBm25Index>)>>>;

/// One index per corpus, keyed by the documents slice pointer (the pin keys its `WeakMap` by array).
fn index_for(documents: &[RecallDocument]) -> std::sync::Arc<RecallBm25Index> {
    static CACHE: RecallBm25IndexCache = Mutex::new(None);
    let key = (documents.as_ptr() as usize, documents.len());
    let mut guard = CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let entries = guard.get_or_insert_with(Vec::new);
    if let Some((_, _, index)) = entries.iter().find(|(ptr, len, _)| (*ptr, *len) == key) {
        return std::sync::Arc::clone(index);
    }
    let index = std::sync::Arc::new(build_index(documents));
    entries.push((key.0, key.1, std::sync::Arc::clone(&index)));
    if entries.len() > 8 {
        entries.remove(0);
    }
    index
}

/// Terms a caller adds to a search, by their distance from the query's own words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecallQueryExpansions {
    pub synonyms: Option<Vec<String>>,
    pub keywords: Option<Vec<String>>,
    pub related: Option<Vec<String>>,
    pub note_line: Option<String>,
}

/// Weight of an added term; the query's own terms weigh 1.0.
pub const RECALL_EXPANSION_WEIGHTS_SYNONYMS: f64 = 0.75;
pub const RECALL_EXPANSION_WEIGHTS_KEYWORDS: f64 = 0.75;
pub const RECALL_EXPANSION_WEIGHTS_RELATED: f64 = 0.4;
pub const RECALL_EXPANSION_WEIGHTS_NOTE_LINE: f64 = 0.4;

/// The weight of each added term; a query term is left out and a term in several tiers keeps its highest.
pub fn recall_expansion_weights(
    expansions: Option<&RecallQueryExpansions>,
    query_terms: &BTreeSet<String>,
) -> BTreeMap<String, f64> {
    let mut weights = BTreeMap::new();
    let Some(expansions) = expansions else {
        return weights;
    };
    let tiers: [(f64, &Option<Vec<String>>); 3] = [
        (RECALL_EXPANSION_WEIGHTS_SYNONYMS, &expansions.synonyms),
        (RECALL_EXPANSION_WEIGHTS_KEYWORDS, &expansions.keywords),
        (RECALL_EXPANSION_WEIGHTS_RELATED, &expansions.related),
    ];
    let note_line = expansions
        .note_line
        .as_ref()
        .map(|line| vec![line.clone()])
        .unwrap_or_default();
    for (weight, texts) in tiers
        .into_iter()
        .map(|(weight, texts)| (weight, texts.clone().unwrap_or_default()))
        .chain([(RECALL_EXPANSION_WEIGHTS_NOTE_LINE, note_line)])
    {
        for text in texts {
            for term in recall_terms(&text) {
                if !query_terms.contains(&term) && weight > weights.get(&term).copied().unwrap_or(0.0) {
                    weights.insert(term, weight);
                }
            }
        }
    }
    weights
}

#[derive(Debug, Clone, PartialEq)]
pub struct RankedRecallDocument {
    pub document: RecallDocument,
    pub score: f64,
    pub full_match: bool,
}

fn query_units(queries: &[String]) -> Vec<String> {
    let mut units: Vec<String> = Vec::new();
    for query in queries {
        for run in runs(query) {
            for piece in run.pieces {
                let term = stem_english_token(&piece);
                if !units.contains(&term) {
                    units.push(term);
                }
            }
        }
    }
    units
}

/// Documents sharing at least one token with the queries, best first; ties break on path.
pub fn rank_recall_documents_bm25(
    documents: &[RecallDocument],
    queries: &[String],
    expansions: Option<&RecallQueryExpansions>,
) -> Vec<RankedRecallDocument> {
    let mut query_terms: Vec<String> = Vec::new();
    for term in queries.iter().flat_map(|query| recall_terms(query)) {
        if !query_terms.contains(&term) {
            query_terms.push(term);
        }
    }
    if query_terms.is_empty() || documents.is_empty() {
        return Vec::new();
    }

    let index = index_for(documents);
    let average_length = if index.average_length > 0.0 {
        index.average_length
    } else {
        1.0
    };
    let document_count = documents.len() as f64;
    let term_score = |term: &str, frequencies: &BTreeMap<String, u64>, length: usize| -> f64 {
        let Some(frequency) = frequencies.get(term) else {
            return 0.0;
        };
        let frequency = *frequency as f64;
        let document_frequency = index.document_frequency.get(term).copied().unwrap_or(0) as f64;
        let idf = (1.0 + (document_count - document_frequency + 0.5) / (document_frequency + 0.5)).ln();
        (idf * frequency * (K1 + 1.0))
            / (frequency + K1 * (1.0 - B + B * length as f64 / average_length))
    };

    let query_set: BTreeSet<String> = query_terms.iter().cloned().collect();
    let added = recall_expansion_weights(expansions, &query_set);
    let units = if added.is_empty() {
        Vec::new()
    } else {
        query_units(queries)
    };

    let mut ranked: Vec<RankedRecallDocument> = Vec::new();
    let mut full_matches: Vec<RankedRecallDocument> = Vec::new();
    for (position, document) in documents.iter().enumerate() {
        let Some(frequencies) = index.term_frequencies.get(position) else {
            continue;
        };
        let length = index.lengths.get(position).copied().unwrap_or(0);
        let mut score: f64 = 0.0;
        for term in &query_terms {
            score += term_score(term, frequencies, length);
        }
        if !added.is_empty() {
            if units.iter().all(|unit| frequencies.contains_key(unit)) {
                full_matches.push(RankedRecallDocument {
                    document: document.clone(),
                    score,
                    full_match: true,
                });
                continue;
            }
            for (term, weight) in &added {
                score += weight * term_score(term, frequencies, length);
            }
        }
        if score > 0.0 {
            ranked.push(RankedRecallDocument {
                document: document.clone(),
                score,
                full_match: false,
            });
        }
    }

    ranked.sort_by(by_score);
    if full_matches.is_empty() {
        return ranked;
    }
    let lift = ranked.first().map(|entry| entry.score).unwrap_or(0.0);
    full_matches.sort_by(by_score);
    for entry in &mut full_matches {
        entry.score += lift;
    }
    full_matches.extend(ranked);
    full_matches
}

fn by_score(left: &RankedRecallDocument, right: &RankedRecallDocument) -> std::cmp::Ordering {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| left.document.path.cmp(&right.document.path))
}

#[cfg(test)]
#[path = "bm25_tests.rs"]
mod tests;
