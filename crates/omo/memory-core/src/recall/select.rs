//! Recall candidate selection (latest `recall/select.ts`).
//!
//! Scores recall documents against the planned queries with the strategy `choose_recall_strategy`
//! picks. The substring path is the FTS-lite AND-semantics scorer and keeps the best hit per path.

use std::cell::OnceCell;
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use unicode_normalization::UnicodeNormalization;

use super::corpus_derived::CorpusDerived;
use super::bm25::{
    RankedRecallPosition, RecallQueryExpansions, is_han_character, rank_recall_positions_bm25,
    recall_expansion_weights, recall_terms, tokenize_recall_text,
};
use super::haystack::normalized_haystack;
use super::provider::RecallDocument;
use super::strategy::{RecallStrategy, choose_recall_strategy_with, has_cjk, haystack_cjk_share};
use crate::search::query::{ParsedQuery, match_score_normalized, normalize_text, parse_query};

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

/// Which ranker produced a candidate, and so which terms center its excerpt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExcerptSource {
    Substring,
    /// `widened` ranks also centered the excerpt on the added terms.
    Bm25 {
        widened: bool,
    },
}

/// A ranked document by position; the excerpt is built only for the candidates finally returned.
#[derive(Debug, Clone, Copy)]
struct RankedCandidate {
    position: usize,
    score: f64,
    excerpt: ExcerptSource,
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
    // Normalized once per cached corpus (else once per selection) and shared by the strategy,
    // substring and phrase-leader paths.
    let haystacks = Haystacks {
        derived: super::corpus_derived::derived_for(documents),
        local: OnceCell::new(),
    };
    let strategy = options.strategy.unwrap_or_else(|| {
        choose_recall_strategy_with(documents.len(), queries, || {
            haystack_cjk_share(haystacks_of(&haystacks, documents))
        })
    });
    let unwidened = rank_by_strategy(
        documents,
        &haystacks,
        queries,
        &parsed_queries,
        options,
        strategy,
    );
    let ranked = if adds_terms(queries, options.expansions.as_ref()) {
        widen_candidates(
            documents,
            &haystacks,
            queries,
            &parsed_queries,
            options,
            strategy,
            unwidened,
        )
    } else {
        unwidened
    };
    let query_terms = collect_query_terms(&parsed_queries);
    let plain_terms: OnceCell<[Vec<String>; 4]> = OnceCell::new();
    let widened_terms: OnceCell<[Vec<String>; 4]> = OnceCell::new();
    ranked
        .into_iter()
        .take(max_items)
        .map(|candidate| {
            let document = &documents[candidate.position];
            let excerpt = match candidate.excerpt {
                ExcerptSource::Substring => build_excerpt(&document.body, &[&query_terms]),
                ExcerptSource::Bm25 { widened } => {
                    let terms = if widened {
                        widened_terms.get_or_init(|| {
                            bm25_excerpt_terms(queries, options.expansions.as_ref())
                        })
                    } else {
                        plain_terms.get_or_init(|| bm25_excerpt_terms(queries, None))
                    };
                    let nfkc_body: String = document.body.nfkc().collect();
                    build_excerpt(&nfkc_body, &[&terms[0], &terms[1], &terms[2], &terms[3]])
                }
            };
            RecallCandidate {
                path: document.path.clone(),
                description: document.description.clone(),
                excerpt,
                score: candidate.score,
            }
        })
        .collect()
}

struct Haystacks {
    derived: Option<Arc<CorpusDerived>>,
    local: OnceCell<Vec<String>>,
}

fn haystacks_of<'a>(haystacks: &'a Haystacks, documents: &[RecallDocument]) -> &'a [String] {
    let build = || documents.iter().map(normalized_haystack).collect();
    match &haystacks.derived {
        Some(derived) => derived.haystacks.get_or_init(build),
        None => haystacks.local.get_or_init(build),
    }
}

fn path_of<'a>(documents: &'a [RecallDocument], candidate: &RankedCandidate) -> &'a str {
    documents[candidate.position].path.as_str()
}

fn expansion_texts(expansions: Option<&RecallQueryExpansions>) -> Vec<String> {
    let Some(expansions) = expansions else {
        return Vec::new();
    };
    let mut texts: Vec<String> = Vec::new();
    for values in [
        &expansions.synonyms,
        &expansions.keywords,
        &expansions.related,
    ].into_iter().flatten() {
        texts.extend(values.iter().cloned());
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
    haystacks: &Haystacks,
    queries: &[String],
    parsed_queries: &[ParsedQuery],
    options: &SelectRecallOptions,
    strategy: RecallStrategy,
) -> Vec<RankedCandidate> {
    match strategy {
        RecallStrategy::Bm25 => rank_bm25(documents, queries, options, None).candidates,
        RecallStrategy::Hybrid => pin_phrase_leader(
            documents,
            fuse_candidates(
                documents,
                &rank_substring_candidates(documents, haystacks, parsed_queries, options),
                &rank_bm25(documents, queries, options, None).candidates,
            ),
            phrase_leader_path(documents, haystacks, parsed_queries, options).as_deref(),
        ),
        RecallStrategy::Substring => {
            rank_substring_candidates(documents, haystacks, parsed_queries, options)
        }
    }
}

/// A widened search: notes holding every query word first, then the widened bm25 order.
fn widen_candidates(
    documents: &[RecallDocument],
    haystacks: &Haystacks,
    queries: &[String],
    parsed_queries: &[ParsedQuery],
    options: &SelectRecallOptions,
    strategy: RecallStrategy,
    unwidened: Vec<RankedCandidate>,
) -> Vec<RankedCandidate> {
    let widened = rank_bm25(documents, queries, options, options.expansions.as_ref());
    let mut holds_every_word: BTreeSet<String> = widened.full_matches.iter().cloned().collect();
    if strategy != RecallStrategy::Bm25 {
        for candidate in rank_substring_candidates(documents, haystacks, parsed_queries, options) {
            holds_every_word.insert(path_of(documents, &candidate).to_string());
        }
    }
    let kept: Vec<RankedCandidate> = unwidened
        .into_iter()
        .filter(|candidate| holds_every_word.contains(path_of(documents, candidate)))
        .collect();
    let kept_paths: BTreeSet<&str> = kept
        .iter()
        .map(|candidate| path_of(documents, candidate))
        .collect();
    let mut combined = kept;
    combined.extend(
        widened
            .candidates
            .into_iter()
            .filter(|candidate| !kept_paths.contains(path_of(documents, candidate))),
    );
    combined
        .into_iter()
        .enumerate()
        .map(|(rank, candidate)| RankedCandidate {
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
    haystacks: &Haystacks,
    parsed_queries: &[ParsedQuery],
    options: &SelectRecallOptions,
) -> Vec<RankedCandidate> {
    let haystacks = haystacks_of(haystacks, documents);
    let mut scored: Vec<RankedCandidate> = Vec::new();
    for (position, (document, haystack)) in documents.iter().zip(haystacks).enumerate() {
        if is_excluded(&document.path, options) {
            continue;
        }
        let mut best: Option<f64> = None;
        for parsed in parsed_queries {
            if let Some(score) = match_score_normalized(haystack, parsed)
                && best.is_none_or(|current| score < current)
            {
                best = Some(score);
            }
        }
        let Some(best) = best else {
            continue;
        };
        scored.push(RankedCandidate {
            position,
            score: best,
            excerpt: ExcerptSource::Substring,
        });
    }
    scored.sort_by(|left, right| {
        left.score
            .partial_cmp(&right.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| path_of(documents, left).cmp(path_of(documents, right)))
    });
    scored
}

struct RankBm25Result {
    candidates: Vec<RankedCandidate>,
    full_matches: Vec<String>,
}

/// The excerpt term lists of the BM25 path: query tokens, Han characters, then the same for the
/// added terms.
fn bm25_excerpt_terms(
    queries: &[String],
    expansions: Option<&RecallQueryExpansions>,
) -> [Vec<String>; 4] {
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
    [
        query_tokens,
        han_characters,
        added_tokens,
        added_han_characters,
    ]
}

/// BM25 path, every match after exclusions, best first; `score` is `1 / (1 + bm25)`.
fn rank_bm25(
    documents: &[RecallDocument],
    queries: &[String],
    options: &SelectRecallOptions,
    expansions: Option<&RecallQueryExpansions>,
) -> RankBm25Result {
    let excerpt = ExcerptSource::Bm25 {
        widened: expansions.is_some(),
    };
    let mut candidates: Vec<RankedCandidate> = Vec::new();
    let mut full_matches: Vec<String> = Vec::new();
    for RankedRecallPosition {
        position,
        score,
        full_match,
    } in rank_recall_positions_bm25(documents, queries, expansions)
    {
        let path = &documents[position].path;
        if is_excluded(path, options) {
            continue;
        }
        if full_match {
            full_matches.push(path.clone());
        }
        candidates.push(RankedCandidate {
            position,
            score: 1.0 / (1.0 + score),
            excerpt,
        });
    }
    RankBm25Result {
        candidates,
        full_matches,
    }
}

struct FusedEntry {
    candidate: RankedCandidate,
    weight: f64,
    bm25_rank: f64,
}

/// Hybrid path: reciprocal rank fusion of the bm25 ranking with the substring matches.
fn fuse_candidates(
    documents: &[RecallDocument],
    substring: &[RankedCandidate],
    bm25: &[RankedCandidate],
) -> Vec<RankedCandidate> {
    let mut fused: Vec<(&str, FusedEntry)> = Vec::new();
    let mut index_of: HashMap<&str, usize> = HashMap::new();
    let mut vote = |candidate: &RankedCandidate, weight: f64, bm25_rank: f64| {
        let path = path_of(documents, candidate);
        if let Some(&index) = index_of.get(path) {
            let entry = &mut fused[index].1;
            entry.weight += weight;
            entry.bm25_rank = entry.bm25_rank.min(bm25_rank);
        } else {
            index_of.insert(path, fused.len());
            fused.push((
                path,
                FusedEntry {
                    candidate: *candidate,
                    weight,
                    bm25_rank,
                },
            ));
        }
    };

    for candidate in substring {
        vote(candidate, 1.0 / (RRF_K + 1.0), f64::INFINITY);
    }
    for (rank, candidate) in bm25.iter().enumerate() {
        vote(candidate, 1.0 / (RRF_K + rank as f64 + 1.0), rank as f64);
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
            .then_with(|| {
                path_of(documents, &left.candidate).cmp(path_of(documents, &right.candidate))
            })
    });
    entries
        .into_iter()
        .map(|entry| RankedCandidate {
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
    haystacks: &Haystacks,
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
    let haystacks = haystacks_of(haystacks, documents);
    let mut leader: Option<(String, f64)> = None;
    for (document, haystack) in documents.iter().zip(haystacks) {
        if is_excluded(&document.path, options) {
            continue;
        }
        for parsed in &multi_word {
            let Some(score) = match_score_normalized(haystack, parsed) else {
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
fn pin_phrase_leader(
    documents: &[RecallDocument],
    fused: Vec<RankedCandidate>,
    leader_path: Option<&str>,
) -> Vec<RankedCandidate> {
    let Some(leader_path) = leader_path else {
        return fused;
    };
    let Some(leader_index) = fused
        .iter()
        .position(|candidate| path_of(documents, candidate) == leader_path)
    else {
        return fused;
    };
    if leader_index == 0 {
        return fused;
    }
    let first_score = fused[0].score;
    let mut leader = fused[leader_index];
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
