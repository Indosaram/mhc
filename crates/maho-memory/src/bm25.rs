//! BM25 ranking over memory documents.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-bm25.md`. Scores are Okapi BM25 (`k1 = 1.5`,
//! `b = 0.75`) over NFKC-normalized, lowercased tokens. Runs of CJK characters are indexed as the
//! run plus one entry per adjacent pair, so a search for two characters finds a longer phrase
//! without a word segmenter. Query terms weigh 1.0; synonym, keyword and related terms weigh less,
//! and a document that matches every query unit is lifted above the ranked list.
use crate::english_stem::stem_english_token;
use std::collections::{BTreeMap, BTreeSet, HashMap};

const K1: f64 = 1.5;
const B: f64 = 0.75;

/// Synonym expansion weight.
pub const WEIGHT_SYNONYMS: f64 = 0.75;
/// Keyword expansion weight.
pub const WEIGHT_KEYWORDS: f64 = 0.75;
/// Related-term expansion weight.
pub const WEIGHT_RELATED: f64 = 0.4;
/// Note-line expansion weight.
pub const WEIGHT_NOTE_LINE: f64 = 0.4;

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

/// True when the character belongs to a CJK run.
pub fn is_cjk_char(ch: char) -> bool {
    is_hangul(ch) || is_han(ch) || is_kana(ch) || ch == '\u{30fc}'
}

/// The characters a single-character CJK token may hold. Only Han, because a lone Hangul or kana
/// character carries too little meaning to index on its own.
pub fn is_han_character(token: &str) -> bool {
    let mut chars = token.chars();
    matches!((chars.next(), chars.next()), (Some(ch), None) if is_han(ch))
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '\u{0300}'..='\u{036F}' | '\u{1AB0}'..='\u{1AFF}')
}

struct Run {
    pieces: Vec<String>,
    han_characters: Vec<String>,
}

fn split_run(token: &str) -> Run {
    let characters: Vec<char> = token.chars().collect();
    let mut pieces = vec![token.to_owned()];
    if characters.len() < 2 || !characters.iter().all(|ch| is_cjk_char(*ch)) {
        return Run {
            pieces,
            han_characters: Vec::new(),
        };
    }
    if characters.len() > 2 {
        for pair in characters.windows(2) {
            pieces.push(pair.iter().collect());
        }
    }
    Run {
        pieces,
        han_characters: characters
            .into_iter()
            .filter(|ch| is_han(*ch))
            .map(String::from)
            .collect(),
    }
}

fn runs(text: &str) -> Vec<Run> {
    let mut normalized = String::with_capacity(text.len());
    for ch in text.chars() {
        for folded in ch.to_lowercase() {
            normalized.push(folded);
        }
    }
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

struct Tokens {
    tokens: Vec<String>,
    standalone_han_characters: usize,
}

fn tokenize(text: &str) -> Tokens {
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
    Tokens {
        tokens,
        standalone_han_characters,
    }
}

/// NFKC-normalized, lowercased tokens with CJK runs expanded into bigrams.
pub fn tokenize_recall_text(text: &str) -> Vec<String> {
    tokenize(text).tokens
}

/// One document's terms and BM25 length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentTermsInput {
    /// The terms, in document order, with repetitions.
    pub terms: Vec<String>,
    /// The length used for length normalization: stand-alone Han characters do not count twice.
    pub length: usize,
}

/// A memory document as the ranker sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecallDocument {
    /// Stable identity used to break score ties.
    pub path: String,
    /// A short description, indexed before the body.
    pub description: String,
    /// The document body.
    pub body: String,
}

/// The terms of one document.
pub fn recall_document_terms(document: &RecallDocument) -> DocumentTermsInput {
    let tokens = tokenize(&format!("{}\n{}", document.description, document.body));
    let length = tokens
        .tokens
        .len()
        .saturating_sub(tokens.standalone_han_characters);
    DocumentTermsInput {
        terms: tokens
            .tokens
            .iter()
            .map(|token| stem_english_token(token))
            .collect(),
        length,
    }
}

/// Query and document terms, lowercased and CJK-expanded.
pub fn recall_terms(text: &str) -> Vec<String> {
    tokenize_recall_text(text)
        .iter()
        .map(|token| stem_english_token(token))
        .collect()
}

/// Terms added to a query, each with the weight it contributes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueryExpansions {
    /// Synonyms of query terms.
    pub synonyms: Option<Vec<String>>,
    /// Keywords attached to the memory.
    pub keywords: Option<Vec<String>>,
    /// Loosely related terms.
    pub related: Option<Vec<String>>,
    /// A single descriptive line.
    pub note_line: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Terms {
    counts: BTreeMap<String, u32>,
    length: usize,
}

struct Index {
    documents: Vec<Terms>,
    document_frequency: HashMap<String, u32>,
    average_length: f64,
}

fn build_index(documents: &[RecallDocument]) -> Index {
    let mut index = Index {
        documents: Vec::with_capacity(documents.len()),
        document_frequency: HashMap::new(),
        average_length: 0.0,
    };
    let mut total = 0usize;
    for document in documents {
        let input = recall_document_terms(document);
        let mut counts: BTreeMap<String, u32> = BTreeMap::new();
        for term in input.terms {
            *counts.entry(term).or_insert(0) += 1;
        }
        for term in counts.keys() {
            *index.document_frequency.entry(term.clone()).or_insert(0) += 1;
        }
        total += input.length;
        index.documents.push(Terms {
            counts,
            length: input.length,
        });
    }
    index.average_length = if documents.is_empty() {
        0.0
    } else {
        total as f64 / documents.len() as f64
    };
    index
}

fn idf(index: &Index, term: &str, document_count: f64) -> f64 {
    let Some(&frequency) = index.document_frequency.get(term) else {
        return 0.0;
    };
    let frequency = f64::from(frequency);
    (1.0 + (document_count - frequency + 0.5) / (frequency + 0.5)).ln()
}

fn term_score(terms: &Terms, term: &str, idf: f64, average_length: f64) -> f64 {
    let Some(&frequency) = terms.counts.get(term) else {
        return 0.0;
    };
    let frequency = f64::from(frequency);
    (idf * frequency * (K1 + 1.0))
        / (frequency + K1 * (1.0 - B + B * terms.length as f64 / average_length))
}

/// One ranked document: its position in the input slice, its score, and whether it matched every
/// query unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RankedDocument {
    /// Position in the slice that was ranked.
    pub position: usize,
    /// The BM25 score, with the full-match lift applied.
    pub score: f64,
    /// Whether every query unit appeared in the document.
    pub full_match: bool,
}

/// The weight of each added term; a term already in the query is left out, and a term that appears
/// in several tiers keeps its highest weight.
pub fn expansion_weights(
    expansions: Option<&QueryExpansions>,
    query_terms: &BTreeSet<String>,
) -> BTreeMap<String, f64> {
    let mut weights = BTreeMap::new();
    let Some(expansions) = expansions else {
        return weights;
    };
    let tiers: [(f64, &Option<Vec<String>>); 3] = [
        (WEIGHT_SYNONYMS, &expansions.synonyms),
        (WEIGHT_KEYWORDS, &expansions.keywords),
        (WEIGHT_RELATED, &expansions.related),
    ];
    let note_line: Vec<String> = expansions.note_line.iter().cloned().collect();
    for (weight, texts) in tiers
        .into_iter()
        .map(|(weight, texts)| (weight, texts.clone().unwrap_or_default()))
        .chain([(WEIGHT_NOTE_LINE, note_line)])
    {
        for text in texts {
            for term in recall_terms(&text) {
                if !query_terms.contains(&term)
                    && weight > weights.get(&term).copied().unwrap_or(0.0)
                {
                    weights.insert(term, weight);
                }
            }
        }
    }
    weights
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

/// Documents sharing at least one term with the queries, best first.
///
/// A document that contains every query unit is ranked above all partial matches and receives the
/// best partial score as a lift, so a page that fully answers the query never loses to one that
/// mentions a single word repeatedly. Ties break on [`RecallDocument::path`], which makes the order
/// stable across runs.
pub fn rank_documents(
    documents: &[RecallDocument],
    queries: &[String],
    expansions: Option<&QueryExpansions>,
) -> Vec<RankedDocument> {
    let mut query_terms: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for term in queries.iter().flat_map(|query| recall_terms(query)) {
        if seen.insert(term.clone()) {
            query_terms.push(term);
        }
    }
    if query_terms.is_empty() || documents.is_empty() {
        return Vec::new();
    }

    let index = build_index(documents);
    let average_length = if index.average_length > 0.0 {
        index.average_length
    } else {
        1.0
    };
    let document_count = documents.len() as f64;
    let query_set: BTreeSet<String> = query_terms.iter().cloned().collect();
    let added: Vec<(String, f64, f64)> = expansion_weights(expansions, &query_set)
        .into_iter()
        .map(|(term, weight)| {
            let idf = idf(&index, &term, document_count);
            (term, weight, idf)
        })
        .collect();
    let query_idf: Vec<(String, f64)> = query_terms
        .iter()
        .map(|term| (term.clone(), idf(&index, term, document_count)))
        .collect();
    let units: Vec<String> = if added.is_empty() {
        Vec::new()
    } else {
        query_units(queries)
    };

    let mut ranked: Vec<RankedDocument> = Vec::new();
    let mut full_matches: Vec<RankedDocument> = Vec::new();
    for (position, terms) in index.documents.iter().enumerate() {
        let mut score = 0.0;
        for (term, idf) in &query_idf {
            score += term_score(terms, term, *idf, average_length);
        }
        if !units.is_empty()
            && units
                .iter()
                .all(|unit| terms.counts.get(unit).copied().unwrap_or(0) > 0)
        {
            full_matches.push(RankedDocument {
                position,
                score,
                full_match: true,
            });
            continue;
        }
        for (term, weight, idf) in &added {
            score += weight * term_score(terms, term, *idf, average_length);
        }
        if score > 0.0 {
            ranked.push(RankedDocument {
                position,
                score,
                full_match: false,
            });
        }
    }

    let by_score = |left: &RankedDocument, right: &RankedDocument| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                documents[left.position]
                    .path
                    .cmp(&documents[right.position].path)
            })
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn document(path: &str, body: &str) -> RecallDocument {
        RecallDocument {
            path: path.to_owned(),
            description: String::new(),
            body: body.to_owned(),
        }
    }

    fn rank(documents: &[RecallDocument], query: &str) -> Vec<usize> {
        rank_documents(documents, &[query.to_owned()], None)
            .into_iter()
            .map(|entry| entry.position)
            .collect()
    }

    #[test]
    fn a_document_that_mentions_the_term_ranks_above_one_that_does_not() {
        let documents = [
            document("a", "sqlite storage"),
            document("b", "unrelated text"),
        ];
        assert_eq!(rank(&documents, "sqlite"), [0]);
    }

    #[test]
    fn repeated_terms_score_higher_than_one_mention() {
        let documents = [
            document("once", "memory"),
            document("many", "memory memory memory"),
        ];
        let ranked = rank_documents(&documents, &["memory".to_owned()], None);
        assert_eq!(ranked[0].position, 1);
        assert!(ranked[0].score > ranked[1].score);
    }

    #[test]
    fn rarer_terms_outweigh_common_ones() {
        let documents = [
            document("common", "the storage"),
            document("rare", "sqlite"),
            document("other", "the storage"),
            document("third", "the storage"),
        ];
        let ranked = rank_documents(&documents, &["the storage sqlite".to_owned()], None);
        assert_eq!(ranked[0].position, 1);
    }

    #[test]
    fn ties_break_on_path_so_the_order_is_stable() {
        let documents = [document("b", "same text"), document("a", "same text")];
        assert_eq!(rank(&documents, "same"), [1, 0]);
    }

    #[test]
    fn an_empty_query_ranks_nothing() {
        let documents = [document("a", "text")];
        assert!(rank(&documents, "   ").is_empty());
    }

    #[test]
    fn an_empty_corpus_ranks_nothing() {
        assert!(rank(&[], "text").is_empty());
    }

    #[test]
    fn a_cjk_phrase_is_found_by_a_fragment_of_it() {
        let documents = [
            document("ko", "메모리 시스템은 오모에서 가져왔다"),
            document("en", "memory system"),
        ];
        let ranked = rank_documents(&documents, &["메모리".to_owned()], None);
        assert_eq!(ranked.first().map(|entry| entry.position), Some(0));
    }

    #[test]
    fn a_han_phrase_is_found_by_two_of_its_characters() {
        let documents = [
            document("zh", "记忆系统很好用"),
            document("other", "unrelated"),
        ];
        let ranked = rank_documents(&documents, &["记忆".to_owned()], None);
        assert_eq!(ranked.first().map(|entry| entry.position), Some(0));
    }

    #[test]
    fn expansion_terms_lift_a_document_that_shares_no_query_term() {
        let documents = [document("a", "daemon worker"), document("b", "unrelated")];
        let expansions = QueryExpansions {
            synonyms: Some(vec!["daemon".to_owned()]),
            ..QueryExpansions::default()
        };
        let without = rank_documents(&documents, &["service".to_owned()], None);
        let with = rank_documents(&documents, &["service".to_owned()], Some(&expansions));
        assert!(without.is_empty());
        assert_eq!(with.first().map(|entry| entry.position), Some(0));
    }

    #[test]
    fn a_query_term_is_never_reweighted_by_an_expansion() {
        let query_terms: BTreeSet<String> = ["memory".to_owned()].into_iter().collect();
        let expansions = QueryExpansions {
            synonyms: Some(vec!["memory".to_owned()]),
            ..QueryExpansions::default()
        };
        assert!(expansion_weights(Some(&expansions), &query_terms).is_empty());
    }

    #[test]
    fn a_term_in_several_tiers_keeps_its_highest_weight() {
        let query_terms = BTreeSet::new();
        let expansions = QueryExpansions {
            synonyms: Some(vec!["service".to_owned()]),
            related: Some(vec!["service".to_owned()]),
            ..QueryExpansions::default()
        };
        let weights = expansion_weights(Some(&expansions), &query_terms);
        let key = stem_english_token("service");
        assert_eq!(weights.get(&key).copied(), Some(WEIGHT_SYNONYMS));
    }

    #[test]
    fn a_full_match_is_lifted_above_a_partial_one() {
        let documents = [
            document("partial", "alpha alpha alpha alpha alpha alpha"),
            document("full", "alpha gamma"),
        ];
        let expansions = QueryExpansions {
            synonyms: Some(vec!["beta".to_owned()]),
            ..QueryExpansions::default()
        };
        let ranked = rank_documents(&documents, &["alpha gamma".to_owned()], Some(&expansions));
        assert_eq!(ranked[0].position, 1);
        assert!(ranked[0].full_match);
        assert!(!ranked[1].full_match);
        assert!(ranked[0].score > ranked[1].score);
    }

    #[test]
    fn the_full_match_lift_only_applies_when_the_query_has_expansions() {
        let documents = [
            document("partial", "alpha alpha alpha alpha alpha alpha"),
            document("full", "alpha gamma"),
        ];
        let ranked = rank_documents(&documents, &["alpha gamma".to_owned()], None);
        assert!(ranked.iter().all(|entry| !entry.full_match));
    }

    #[test]
    fn the_length_does_not_count_standalone_han_twice() {
        let terms = recall_document_terms(&document("zh", "记忆"));
        assert!(terms.length < terms.terms.len());
    }

    #[test]
    fn a_single_han_character_is_recognized_and_a_latin_one_is_not() {
        assert!(is_han_character("记"));
        assert!(!is_han_character("a"));
        assert!(!is_han_character("记忆"));
    }

    #[test]
    fn scoring_is_independent_of_the_document_order_in_the_slice() {
        let documents = [document("a", "memory store"), document("b", "memory bank")];
        let first = rank_documents(&documents, &["memory".to_owned()], None);
        let scores: Vec<f64> = first.iter().map(|entry| entry.score).collect();
        let reversed = [documents[1].clone(), documents[0].clone()];
        let second = rank_documents(&reversed, &["memory".to_owned()], None);
        let mut second_scores: Vec<f64> = second.iter().map(|entry| entry.score).collect();
        second_scores.sort_by(|left, right| right.partial_cmp(left).expect("ordered"));
        let mut first_scores = scores;
        first_scores.sort_by(|left, right| right.partial_cmp(left).expect("ordered"));
        assert_eq!(first_scores, second_scores);
    }

    #[test]
    fn a_plural_query_finds_a_singular_document() {
        let documents = [
            document("a", "the memory system"),
            document("b", "unrelated words"),
        ];
        let ranked = rank_documents(&documents, &["memories".to_owned()], None);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].position, 0);
    }

    #[test]
    fn an_inflected_query_finds_the_base_word() {
        let documents = [
            document("a", "we recorded the decision"),
            document("b", "nothing here"),
        ];
        let ranked = rank_documents(&documents, &["recording".to_owned()], None);
        assert_eq!(ranked.len(), 1);
    }
}
