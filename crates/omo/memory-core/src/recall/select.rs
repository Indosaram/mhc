//! Recall candidate selection (latest `recall/select.ts`).
//!
//! Scores recall documents against the planned queries with the strategy `choose_recall_strategy`
//! picks. The substring path is the FTS-lite AND-semantics scorer and keeps the best hit per path.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use unicode_normalization::UnicodeNormalization;

use super::bm25::{
    RankedRecallDocument, RecallQueryExpansions, is_han_character, rank_recall_documents_bm25,
    recall_expansion_weights, recall_terms, tokenize_recall_text,
};
use super::haystack::normalized_haystack;
use super::provider::RecallDocument;
use super::strategy::{RecallStrategy, choose_recall_strategy, has_cjk};
use crate::search::query::{ParsedQuery, match_score, normalize_text, parse_query};

/// One scored recall candidate; `score` is ascending (lower is better).
#[derive(Debug, Clone, PartialEq)]
pub struct RecallCandidate {
    pub path: String,
    pub description: String,
    pub excerpt: String,
    pub score: f64,
}

/// Excerpt window length. Internal, deliberately not a config knob.
const EXCERPT_CHARS: usize = 200;

/// Reciprocal rank fusion constant (Cormack et al.); 60 is the customary value.
const RRF_K: f64 = 60.0;

/// Options for one selection pass.
pub struct SelectRecallOptions {
    pub max_items: usize,
    /// Paths already surfaced earlier in the session; they never repeat.
    pub surfaced: BTreeSet<String>,
    /// Additional paths to skip, such as memories already visible in the transcript.
    pub exclude_paths: Option<BTreeSet<String>>,
    /// Internal override for tests and the benchmark; chosen by `choose_recall_strategy` when absent.
    pub strategy: Option<RecallStrategy>,
    /// Terms the caller adds to this search, scored below the queries' own words.
    pub expansions: Option<RecallQueryExpansions>,
}

/// Selects the best recall candidates for the planned queries.
pub fn select_recall_candidates(
    documents: &[RecallDocument],
    queries: &[String],
    options: &SelectRecallOptions,
) -> Vec<RecallCandidate> {
    let max_items = options.max_items;
    let parsed_queries: Vec<ParsedQuery> = queries
        .iter()
        .map(|query| parse_query(query))
        .filter(|parsed| !parsed.terms.is_empty() || !parsed.phrases.is_empty())
        .collect();
    if max_items == 0 || parsed_queries.is_empty() {
        return Vec::new();
    }
    let strategy = options
        .strategy
        .unwrap_or_else(|| choose_recall_strategy(documents, queries));
    let unwidened = rank_by_strategy(documents, queries, &parsed_queries, options, strategy);
    if !adds_terms(queries, options.expansions.as_ref()) {
        return unwidened.into_iter().take(max_items).collect();
    }
    widen_candidates(documents, queries, &parsed_queries, options, strategy, unwidened)
        .into_iter()
        .take(max_items)
        .collect()
}

fn expansion_texts(expansions: Option<&RecallQueryExpansions>) -> Vec<String> {
    let Some(expansions) = expansions else {
        return Vec::new();
    };
    let mut texts: Vec<String> = Vec::new();
    for tier in [
        &expansions.synonyms,
        &expansions.keywords,
        &expansions.related,
    ] {
        if let Some(values) = tier {
            texts.extend(values.iter().cloned());
        }
    }
    if let Some(note_line) = &expansions.note_line {
        texts.push(note_line.clone());
    }
    texts.retain(|text| !text.trim().is_empty());
    texts
}

/// Whether the expansions add a term the queries do not already hold.
fn adds_terms(queries: &[String], expansions: Option<&RecallQueryExpansions>) -> bool {
    if expansion_texts(expansions).is_empty() {
        return false;
    }
    let mut query_terms: BTreeSet<String> = BTreeSet::new();
    for query in queries {
        query_terms.extend(recall_terms(query));
    }
    !recall_expansion_weights(expansions, &query_terms).is_empty()
}

/// Every candidate of the search as written, best first; callers apply the cap.
fn rank_by_strategy(
    documents: &[RecallDocument],
    queries: &[String],
    parsed_queries: &[ParsedQuery],
    options: &SelectRecallOptions,
    strategy: RecallStrategy,
) -> Vec<RecallCandidate> {
    match strategy {
        RecallStrategy::Bm25 => rank_bm25(documents, queries, options, None).candidates,
        RecallStrategy::Hybrid => pin_phrase_leader(
            fuse_candidates(
                &rank_substring_candidates(documents, parsed_queries, options),
                &rank_bm25(documents, queries, options, None).candidates,
            ),
            phrase_leader_path(documents, parsed_queries, options).as_deref(),
        ),
        RecallStrategy::Substring => rank_substring_candidates(documents, parsed_queries, options),
    }
}

/// A widened search: notes holding every query word first, then the widened bm25 order.
fn widen_candidates(
    documents: &[RecallDocument],
    queries: &[String],
    parsed_queries: &[ParsedQuery],
    options: &SelectRecallOptions,
    strategy: RecallStrategy,
    unwidened: Vec<RecallCandidate>,
) -> Vec<RecallCandidate> {
    let widened = rank_bm25(documents, queries, options, options.expansions.as_ref());
    let mut holds_every_word: BTreeSet<String> = widened.full_matches.iter().cloned().collect();
    if strategy != RecallStrategy::Bm25 {
        for candidate in rank_substring_candidates(documents, parsed_queries, options) {
            holds_every_word.insert(candidate.path);
        }
    }
    let kept: Vec<RecallCandidate> = unwidened
        .into_iter()
        .filter(|candidate| holds_every_word.contains(&candidate.path))
        .collect();
    let kept_paths: BTreeSet<String> = kept.iter().map(|candidate| candidate.path.clone()).collect();
    let mut combined = kept;
    combined.extend(
        widened
            .candidates
            .into_iter()
            .filter(|candidate| !kept_paths.contains(&candidate.path)),
    );
    combined
        .into_iter()
        .enumerate()
        .map(|(rank, candidate)| RecallCandidate {
            score: 1.0 / (1.0 + 1.0 / (RRF_K + rank as f64 + 1.0)),
            ..candidate
        })
        .collect()
}

fn is_excluded(path: &str, options: &SelectRecallOptions) -> bool {
    options.surfaced.contains(path)
        || options
            .exclude_paths
            .as_ref()
            .is_some_and(|excluded| excluded.contains(path))
}

/// Every substring match after exclusions, best first; callers apply the cap.
fn rank_substring_candidates(
    documents: &[RecallDocument],
    parsed_queries: &[ParsedQuery],
    options: &SelectRecallOptions,
) -> Vec<RecallCandidate> {
    let query_terms = collect_query_terms(parsed_queries);
    let mut scored: Vec<RecallCandidate> = Vec::new();
    for document in documents {
        if is_excluded(&document.path, options) {
            continue;
        }
        let haystack = normalized_haystack(document);
        let mut best: Option<f64> = None;
        for parsed in parsed_queries {
            if let Some(score) = match_score(&haystack, parsed)
                && best.is_none_or(|current| score < current)
            {
                best = Some(score);
            }
        }
        let Some(best) = best else {
            continue;
        };
        scored.push(RecallCandidate {
            path: document.path.clone(),
            description: document.description.clone(),
            excerpt: build_excerpt(&document.body, &[&query_terms]),
            score: best,
        });
    }
    scored.sort_by(|left, right| {
        left.score
            .partial_cmp(&right.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.path.cmp(&right.path))
    });
    scored
}

struct RankBm25Result {
    candidates: Vec<RecallCandidate>,
    full_matches: Vec<String>,
}

/// BM25 path, every match after exclusions, best first; `score` is `1 / (1 + bm25)`.
fn rank_bm25(
    documents: &[RecallDocument],
    queries: &[String],
    options: &SelectRecallOptions,
    expansions: Option<&RecallQueryExpansions>,
) -> RankBm25Result {
    let mut tokens: Vec<String> = Vec::new();
    for query in queries {
        for token in tokenize_recall_text(query) {
            if !tokens.contains(&token) {
                tokens.push(token);
            }
        }
    }
    let query_tokens: Vec<String> = tokens
        .iter()
        .filter(|token| token.chars().count() > 1)
        .cloned()
        .collect();
    let han_characters: Vec<String> = tokens
        .iter()
        .filter(|token| is_han_character(token))
        .cloned()
        .collect();

    let mut added: Vec<String> = Vec::new();
    for text in expansion_texts(expansions) {
        for token in tokenize_recall_text(&text) {
            if !added.contains(&token) {
                added.push(token);
            }
        }
    }
    let added_tokens: Vec<String> = added
        .iter()
        .filter(|token| token.chars().count() > 1)
        .cloned()
        .collect();
    let added_han_characters: Vec<String> = added
        .iter()
        .filter(|token| is_han_character(token))
        .cloned()
        .collect();

    let mut candidates: Vec<RecallCandidate> = Vec::new();
    let mut full_matches: Vec<String> = Vec::new();
    for RankedRecallDocument {
        document,
        score,
        full_match,
    } in rank_recall_documents_bm25(documents, queries, expansions)
    {
        if is_excluded(&document.path, options) {
            continue;
        }
        if full_match {
            full_matches.push(document.path.clone());
        }
        let nfkc_body: String = document.body.nfkc().collect();
        candidates.push(RecallCandidate {
            path: document.path.clone(),
            description: document.description.clone(),
            excerpt: build_excerpt(
                &nfkc_body,
                &[
                    &query_tokens,
                    &han_characters,
                    &added_tokens,
                    &added_han_characters,
                ],
            ),
            score: 1.0 / (1.0 + score),
        });
    }
    RankBm25Result {
        candidates,
        full_matches,
    }
}

struct FusedEntry {
    candidate: RecallCandidate,
    weight: f64,
    bm25_rank: f64,
}

/// Hybrid path: reciprocal rank fusion of the bm25 ranking with the substring matches.
fn fuse_candidates(substring: &[RecallCandidate], bm25: &[RecallCandidate]) -> Vec<RecallCandidate> {
    fn vote(fused: &mut Vec<(String, FusedEntry)>, candidate: &RecallCandidate, weight: f64, bm25_rank: f64) {
        if let Some((_, entry)) = fused
            .iter_mut()
            .find(|(path, _)| path == &candidate.path)
        {
            entry.weight += weight;
            entry.bm25_rank = entry.bm25_rank.min(bm25_rank);
        } else {
            fused.push((
                candidate.path.clone(),
                FusedEntry {
                    candidate: candidate.clone(),
                    weight,
                    bm25_rank,
                },
            ));
        }
    }

    let mut fused: Vec<(String, FusedEntry)> = Vec::new();
    for candidate in substring {
        vote(&mut fused, candidate, 1.0 / (RRF_K + 1.0), f64::INFINITY);
    }
    for (rank, candidate) in bm25.iter().enumerate() {
        vote(
            &mut fused,
            candidate,
            1.0 / (RRF_K + rank as f64 + 1.0),
            rank as f64,
        );
    }
    let mut entries: Vec<FusedEntry> = fused.into_iter().map(|(_, entry)| entry).collect();
    entries.sort_by(|left, right| {
        right
            .weight
            .partial_cmp(&left.weight)
            .unwrap_or(Ordering::Equal)
            .then_with(|| {
                left.bm25_rank
                    .partial_cmp(&right.bm25_rank)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| left.candidate.path.cmp(&right.candidate.path))
    });
    entries
        .into_iter()
        .map(|entry| RecallCandidate {
            score: 1.0 / (1.0 + entry.weight),
            ..entry.candidate
        })
        .collect()
}

fn is_non_cjk_multi_word(parsed: &ParsedQuery) -> bool {
    let multi_word = parsed.terms.len() + parsed.phrases.len() > 1
        || parsed
            .phrases
            .iter()
            .any(|phrase| phrase.trim().contains(char::is_whitespace));
    if !multi_word {
        return false;
    }
    let joined = parsed
        .terms
        .iter()
        .chain(parsed.phrases.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    !has_cjk(&joined)
}

/// The note today's substring ranker puts first among non-CJK multi-word query matches.
fn phrase_leader_path(
    documents: &[RecallDocument],
    parsed_queries: &[ParsedQuery],
    options: &SelectRecallOptions,
) -> Option<String> {
    let multi_word: Vec<&ParsedQuery> = parsed_queries
        .iter()
        .filter(|parsed| is_non_cjk_multi_word(parsed))
        .collect();
    if multi_word.is_empty() {
        return None;
    }
    let mut leader: Option<(String, f64)> = None;
    for document in documents {
        if is_excluded(&document.path, options) {
            continue;
        }
        let haystack = normalized_haystack(document);
        for parsed in &multi_word {
            let Some(score) = match_score(&haystack, parsed) else {
                continue;
            };
            match &leader {
                None => leader = Some((document.path.clone(), score)),
                Some((path, best)) => {
                    if score < *best || (score == *best && document.path < *path) {
                        leader = Some((document.path.clone(), score));
                    }
                }
            }
        }
    }
    leader.map(|(path, _)| path)
}

/// Substring floor for the hybrid: the phrase leader keeps the first place it holds today.
fn pin_phrase_leader(fused: Vec<RecallCandidate>, leader_path: Option<&str>) -> Vec<RecallCandidate> {
    let Some(leader_path) = leader_path else {
        return fused;
    };
    let Some(leader_index) = fused.iter().position(|candidate| candidate.path == leader_path) else {
        return fused;
    };
    if leader_index == 0 {
        return fused;
    }
    let first_score = fused[0].score;
    let mut leader = fused[leader_index].clone();
    leader.score = first_score;
    let mut ordered = Vec::with_capacity(fused.len());
    ordered.push(leader);
    for (index, candidate) in fused.into_iter().enumerate() {
        if index != leader_index {
            ordered.push(candidate);
        }
    }
    ordered
}

fn collect_query_terms(parsed_queries: &[ParsedQuery]) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for parsed in parsed_queries {
        terms.extend(parsed.terms.iter().cloned());
        for phrase in &parsed.phrases {
            terms.extend(
                phrase
                    .split_whitespace()
                    .filter(|part| !part.is_empty())
                    .map(str::to_string),
            );
        }
    }
    terms
}

/// Excerpt centered on the first query-term match, or the body head when no term matches.
fn build_excerpt(body: &str, term_lists: &[&[String]]) -> String {
    let normalized = collapse_whitespace(body);
    if normalized.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = normalized.chars().collect();
    let lowered: Vec<char> = normalized.to_lowercase().chars().collect();

    let mut match_index: Option<usize> = None;
    let mut match_length = 0usize;
    for list in term_lists {
        if match_index.is_some() {
            break;
        }
        for term in *list {
            let needle = normalize_text(term);
            if needle.is_empty() {
                continue;
            }
            let needle_chars: Vec<char> = needle.chars().collect();
            if let Some(index) = find_chars(&lowered, &needle_chars)
                && match_index.is_none_or(|current| index < current)
            {
                match_index = Some(index);
                match_length = needle_chars.len();
            }
        }
    }

    let Some(match_index) = match_index else {
        return chars.iter().take(EXCERPT_CHARS).collect();
    };

    let half = EXCERPT_CHARS.saturating_sub(match_length) / 2;
    let start = match_index.saturating_sub(half);
    let end = (start + EXCERPT_CHARS).min(chars.len());
    let clamped_start = end.saturating_sub(EXCERPT_CHARS);
    chars[clamped_start..end]
        .iter()
        .collect::<String>()
        .trim()
        .to_string()
}

fn find_chars(haystack: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&start| &haystack[start..start + needle.len()] == needle)
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod tests;
