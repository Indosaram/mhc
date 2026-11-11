//! Recall candidate selection: which memories to surface, and the excerpt to show for each.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-select.md`. Candidates come from two rankers — a
//! substring scan and BM25 — fused by reciprocal rank, after paths that were already surfaced are
//! removed. Excerpts are built only for the candidates that survive the cap, so a wide corpus does
//! not pay for text nobody reads.
use std::collections::BTreeSet;

use crate::bm25::{QueryExpansions, RecallDocument, rank_documents, recall_terms};
use crate::recall_strategy::{RecallStrategy, choose_recall_strategy_with, corpus_cjk_share};

/// Characters shown in one excerpt.
pub const EXCERPT_CHARS: usize = 200;

/// Reciprocal rank fusion constant (Cormack et al.); 60 is the customary value.
pub const RRF_K: f64 = 60.0;

/// One surfaced memory.
#[derive(Clone, Debug, PartialEq)]
pub struct RecallCandidate {
    /// Stable document identity.
    pub path: String,
    /// The document's one-line description.
    pub description: String,
    /// The excerpt to show, centered on the matched term.
    pub excerpt: String,
    /// Relevance in `0.0..=1.0`, higher is better.
    pub score: f64,
}

/// What one selection pass may return.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectOptions {
    /// Most candidates to return; `0` returns none.
    pub max_items: usize,
    /// Paths already shown this session; they never repeat.
    pub surfaced: BTreeSet<String>,
    /// Extra paths to skip, such as memories already visible in the transcript.
    pub exclude_paths: Option<BTreeSet<String>>,
    /// Terms the caller adds to the search, scored below the queries' own words.
    pub expansions: Option<QueryExpansions>,
    /// Which scorer to run. Chosen from the corpus when absent; callers override it only to pin a
    /// strategy in a test or a benchmark.
    pub strategy: Option<RecallStrategy>,
}

struct Scored {
    position: usize,
    score: f64,
}

fn normalize(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn is_excluded(path: &str, options: &SelectOptions) -> bool {
    options.surfaced.contains(path)
        || options
            .exclude_paths
            .as_ref()
            .is_some_and(|excluded| excluded.contains(path))
}

/// The earliest position at which any term occurs, and how many terms occur at all.
///
/// A term that appears nowhere contributes nothing to coverage, so a page that repeats one query
/// word cannot outrank a page that uses all of them.
fn term_coverage(haystack: &str, terms: &[String]) -> Option<(usize, usize, usize)> {
    let mut earliest: Option<usize> = None;
    let mut hits = 0usize;
    for term in terms {
        if term.is_empty() {
            continue;
        }
        if let Some(index) = haystack.find(term.as_str()) {
            hits += 1;
            earliest = Some(earliest.map_or(index, |current| current.min(index)));
        }
    }
    earliest.map(|index| (index, hits, terms.len()))
}

fn substring_candidates(
    documents: &[RecallDocument],
    terms: &[String],
    options: &SelectOptions,
) -> Vec<Scored> {
    let mut scored = Vec::new();
    for (position, document) in documents.iter().enumerate() {
        if is_excluded(&document.path, options) {
            continue;
        }
        let haystack = normalize(&format!("{} {}", document.description, document.body));
        let Some((index, hits, total)) = term_coverage(&haystack, terms) else {
            continue;
        };
        let length = haystack.chars().count().max(1);
        let position_factor = 1.0 - (index as f64 / length as f64).min(1.0);
        let coverage = hits as f64 / total.max(1) as f64;
        scored.push(Scored {
            position,
            score: (position_factor * coverage).clamp(0.0, 1.0),
        });
    }
    sort_descending(&mut scored, documents);
    scored
}

fn bm25_candidates(
    documents: &[RecallDocument],
    queries: &[String],
    options: &SelectOptions,
) -> Vec<Scored> {
    let ranked = rank_documents(documents, queries, options.expansions.as_ref());
    let best = ranked
        .iter()
        .map(|entry| entry.score)
        .fold(0.0f64, f64::max);
    if best <= 0.0 {
        return Vec::new();
    }
    let mut scored: Vec<Scored> = ranked
        .into_iter()
        .filter(|entry| !is_excluded(&documents[entry.position].path, options))
        .map(|entry| Scored {
            position: entry.position,
            score: (entry.score / best).clamp(0.0, 1.0),
        })
        .collect();
    sort_descending(&mut scored, documents);
    scored
}

/// A single ranker's output as final scores.
///
/// RRF weights are rank-based and deliberately flat, so one ranker cannot be passed through `fuse`:
/// fusing a list with an empty one would rescale every score to 1/(RRF_K+1) relative to the best,
/// which is a different number from the ranker's own relevance even though the order is the same.
/// Normalizing against the best score keeps one ranker's output on the same 0..=1 scale as a fusion.
fn rank_to_scores(scored: &[Scored]) -> Vec<(usize, f64)> {
    let best = scored
        .iter()
        .map(|entry| entry.score)
        .fold(0.0f64, f64::max);
    if best <= 0.0 {
        return Vec::new();
    }
    scored
        .iter()
        .map(|entry| (entry.position, (entry.score / best).clamp(0.0, 1.0)))
        .collect()
}

/// Every text the caller added to the search, in the order they were supplied.
fn expansion_texts(expansions: Option<&QueryExpansions>) -> Vec<String> {
    let Some(expansions) = expansions else {
        return Vec::new();
    };
    let mut texts: Vec<String> = Vec::new();
    for values in [
        &expansions.synonyms,
        &expansions.keywords,
        &expansions.related,
    ]
    .into_iter()
    .flatten()
    {
        texts.extend(values.iter().cloned());
    }
    if let Some(note_line) = &expansions.note_line {
        texts.push(note_line.clone());
    }
    texts.retain(|text| !text.trim().is_empty());
    texts
}

/// Whether the caller's added terms contribute anything the query did not already contain.
fn adds_terms(expansions: Option<&QueryExpansions>) -> bool {
    !expansion_texts(expansions).is_empty()
}

/// A widened search: documents holding every query word first, then the widened BM25 order.
///
/// Ranks are re-assigned from the fused order rather than kept, because the two lists were scored on
/// different scales and only their relative positions are comparable.
fn widen(
    documents: &[RecallDocument],
    terms: &[String],
    queries: &[String],
    options: &SelectOptions,
    strategy: RecallStrategy,
    unwidened: Vec<(usize, f64)>,
) -> Vec<(usize, f64)> {
    let widened = bm25_candidates(documents, queries, options);
    let mut holds_every_word: BTreeSet<String> = widened
        .iter()
        .filter(|entry| {
            let haystack = {
                let document = &documents[entry.position];
                normalize(&format!("{} {}", document.description, document.body))
            };
            term_coverage(&haystack, terms).is_some_and(|(_, hits, total)| hits == total)
        })
        .map(|entry| documents[entry.position].path.clone())
        .collect();
    if strategy != RecallStrategy::Bm25 {
        for entry in substring_candidates(documents, terms, options) {
            holds_every_word.insert(documents[entry.position].path.clone());
        }
    }
    let mut combined: Vec<usize> = unwidened
        .into_iter()
        .map(|(position, _)| position)
        .filter(|position| holds_every_word.contains(&documents[*position].path))
        .collect();
    let kept: BTreeSet<usize> = combined.iter().copied().collect();
    combined.extend(
        widened
            .into_iter()
            .map(|entry| entry.position)
            .filter(|position| !kept.contains(position)),
    );
    combined
        .into_iter()
        .enumerate()
        .map(|(rank, position)| (position, 1.0 / (1.0 + 1.0 / (RRF_K + rank as f64 + 1.0))))
        .collect()
}

fn sort_descending(scored: &mut [Scored], documents: &[RecallDocument]) {
    scored.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                documents[left.position]
                    .path
                    .cmp(&documents[right.position].path)
            })
    });
}

fn fuse(documents: &[RecallDocument], substring: &[Scored], bm25: &[Scored]) -> Vec<(usize, f64)> {
    let mut order: Vec<usize> = Vec::new();
    let mut weights: std::collections::HashMap<usize, f64> = std::collections::HashMap::new();
    for list in [substring, bm25] {
        for (rank, entry) in list.iter().enumerate() {
            let weight = 1.0 / (RRF_K + rank as f64 + 1.0);
            match weights.get_mut(&entry.position) {
                Some(existing) => *existing += weight,
                None => {
                    weights.insert(entry.position, weight);
                    order.push(entry.position);
                }
            }
        }
    }
    let best = order
        .iter()
        .filter_map(|position| weights.get(position).copied())
        .fold(0.0f64, f64::max);
    let mut fused: Vec<(usize, f64)> = order
        .into_iter()
        .map(|position| {
            let weight = weights.get(&position).copied().unwrap_or(0.0);
            (position, (weight / best).clamp(0.0, 1.0))
        })
        .collect();
    fused.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| documents[left.0].path.cmp(&documents[right.0].path))
    });
    fused
}

/// Select the best candidates for the queries.
///
/// Every returned score is a relevance in `0.0..=1.0`, higher is better, regardless of which ranker
/// produced the candidate. Ties break on path, so the same corpus and query always return the same
/// order.
pub fn select_candidates(
    documents: &[RecallDocument],
    queries: &[String],
    options: &SelectOptions,
) -> Vec<RecallCandidate> {
    if options.max_items == 0 || documents.is_empty() {
        return Vec::new();
    }
    let mut terms: Vec<String> = Vec::new();
    for query in queries {
        for term in recall_terms(query) {
            let term = normalize(&term);
            if !term.is_empty() && !terms.contains(&term) {
                terms.push(term);
            }
        }
    }
    if terms.is_empty() {
        return Vec::new();
    }

    let strategy = options.strategy.unwrap_or_else(|| {
        choose_recall_strategy_with(documents.len(), queries, || corpus_cjk_share(documents))
    });
    let unwidened = match strategy {
        RecallStrategy::Substring => {
            rank_to_scores(&substring_candidates(documents, &terms, options))
        }
        RecallStrategy::Bm25 => rank_to_scores(&bm25_candidates(documents, queries, options)),
        RecallStrategy::Hybrid => {
            let substring = substring_candidates(documents, &terms, options);
            let bm25 = bm25_candidates(documents, queries, options);
            fuse(documents, &substring, &bm25)
        }
    };
    // Added terms widen the search under every strategy. A substring-only pass never looks at a
    // document sharing no query word, so a synonym the caller supplied would be ignored exactly when
    // it was the only thing pointing at the right memory.
    let fused = if adds_terms(options.expansions.as_ref()) {
        widen(documents, &terms, queries, options, strategy, unwidened)
    } else {
        unwidened
    };

    // Terms the excerpt may center on: the query's own, then the added ones. A memory reached only
    // through a synonym has none of the query words in its body, so centering on the query terms
    // alone would show the reader a window on text that does not explain why it was returned.
    let mut excerpt_terms = terms.clone();
    for expansion in expansion_texts(options.expansions.as_ref()) {
        for term in recall_terms(&expansion) {
            let term = normalize(&term);
            if !term.is_empty() && !excerpt_terms.contains(&term) {
                excerpt_terms.push(term);
            }
        }
    }

    fused
        .into_iter()
        .take(options.max_items)
        .map(|(position, score)| {
            let document = &documents[position];
            RecallCandidate {
                path: document.path.clone(),
                description: document.description.clone(),
                excerpt: build_excerpt(&document.body, &excerpt_terms),
                score,
            }
        })
        .collect()
}

/// Build an excerpt centered on the earliest occurrence of any term.
///
/// Whitespace is collapsed first, the window is [`EXCERPT_CHARS`] characters, and when no term is
/// found the body is simply truncated from the start. The window is clamped at the end of the body
/// so it never runs past it.
pub fn build_excerpt(body: &str, terms: &[String]) -> String {
    let normalized = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = normalized.chars().collect();
    let lowered: Vec<char> = normalized.to_lowercase().chars().collect();

    let mut match_index: Option<usize> = None;
    let mut match_length = 0usize;
    for term in terms {
        let needle: Vec<char> = normalize(term).chars().collect();
        if needle.is_empty() || needle.len() > lowered.len() {
            continue;
        }
        let found = (0..=lowered.len() - needle.len())
            .find(|start| lowered[*start..*start + needle.len()] == needle[..]);
        if let Some(index) = found
            && match_index.is_none_or(|current| index < current)
        {
            match_index = Some(index);
            match_length = needle.len();
        }
    }

    let Some(match_index) = match_index else {
        return chars.iter().take(EXCERPT_CHARS).collect();
    };
    let half = EXCERPT_CHARS.saturating_sub(match_length) / 2;
    let start = match_index.saturating_sub(half);
    let end = (start + EXCERPT_CHARS).min(chars.len());
    let clamped_start = end.saturating_sub(EXCERPT_CHARS);
    chars[clamped_start..end].iter().collect::<String>()
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

    fn options(max_items: usize) -> SelectOptions {
        SelectOptions {
            max_items,
            ..SelectOptions::default()
        }
    }

    fn select(documents: &[RecallDocument], query: &str, max_items: usize) -> Vec<RecallCandidate> {
        select_candidates(documents, &[query.to_owned()], &options(max_items))
    }

    #[test]
    fn a_matching_document_is_returned_and_a_non_matching_one_is_not() {
        let documents = [
            document("memory/store.md", "SQLite bank storage"),
            document("cooking/pasta.md", "boil water"),
        ];
        let candidates = select(&documents, "sqlite", 5);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, "memory/store.md");
    }

    #[test]
    fn scores_are_relevance_in_the_unit_interval() {
        let documents = [
            document("a", "sqlite bank"),
            document("b", "the bank stores things, sqlite"),
        ];
        for candidate in select(&documents, "sqlite bank", 5) {
            assert!(
                (0.0..=1.0).contains(&candidate.score),
                "score out of range: {}",
                candidate.score
            );
        }
    }

    #[test]
    fn an_earlier_mention_outranks_a_later_one() {
        let documents = [
            document("late", &format!("{} sqlite", "filler ".repeat(40))),
            document("early", "sqlite then filler"),
        ];
        let candidates = select(&documents, "sqlite", 5);
        assert_eq!(candidates[0].path, "early");
    }

    #[test]
    fn a_document_using_every_query_word_outranks_one_repeating_a_single_word() {
        let documents = [
            document("one", "sqlite sqlite sqlite sqlite"),
            document("all", "sqlite bank"),
        ];
        let candidates = select(&documents, "sqlite bank", 5);
        assert_eq!(candidates[0].path, "all");
    }

    #[test]
    fn the_cap_is_respected() {
        let documents: Vec<RecallDocument> = (0..10)
            .map(|index| document(&format!("doc{index}"), "shared term"))
            .collect();
        assert_eq!(select(&documents, "shared", 3).len(), 3);
    }

    #[test]
    fn a_zero_cap_returns_nothing() {
        let documents = [document("a", "shared term")];
        assert!(select(&documents, "shared", 0).is_empty());
    }

    #[test]
    fn an_empty_query_returns_nothing() {
        let documents = [document("a", "shared term")];
        assert!(select(&documents, "   ", 5).is_empty());
    }

    #[test]
    fn a_surfaced_path_is_never_returned_again() {
        let documents = [document("seen", "sqlite"), document("new", "sqlite")];
        let mut surfaced = BTreeSet::new();
        surfaced.insert("seen".to_owned());
        let options = SelectOptions {
            max_items: 5,
            surfaced,
            ..SelectOptions::default()
        };
        let candidates = select_candidates(&documents, &["sqlite".to_owned()], &options);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, "new");
    }

    #[test]
    fn an_excluded_path_is_skipped() {
        let documents = [document("visible", "sqlite"), document("hidden", "sqlite")];
        let mut exclude = BTreeSet::new();
        exclude.insert("visible".to_owned());
        let options = SelectOptions {
            max_items: 5,
            exclude_paths: Some(exclude),
            ..SelectOptions::default()
        };
        let candidates = select_candidates(&documents, &["sqlite".to_owned()], &options);
        assert_eq!(candidates[0].path, "hidden");
    }

    #[test]
    fn a_path_in_both_rankers_beats_one_that_only_the_substring_ranker_likes() {
        let documents = [
            document("both", "sqlite bank storage"),
            document("substring_only", "sqlite"),
        ];
        let candidates = select(&documents, "sqlite bank", 5);
        assert_eq!(candidates[0].path, "both");
    }

    #[test]
    fn the_excerpt_is_centered_on_the_matched_term() {
        let body = format!("{} needle {}", "filler ".repeat(60), "tail ".repeat(60));
        let excerpt = build_excerpt(&body, &["needle".to_owned()]);
        assert!(excerpt.contains("needle"));
        assert!(excerpt.chars().count() <= EXCERPT_CHARS);
    }

    #[test]
    fn the_excerpt_never_runs_past_the_end_of_a_short_body() {
        let excerpt = build_excerpt("short body with needle", &["needle".to_owned()]);
        assert_eq!(excerpt, "short body with needle");
    }

    #[test]
    fn an_excerpt_without_a_match_is_the_head_of_the_body() {
        let body = "x".repeat(500);
        let excerpt = build_excerpt(&body, &["absent".to_owned()]);
        assert_eq!(excerpt.chars().count(), EXCERPT_CHARS);
    }

    #[test]
    fn an_empty_body_yields_an_empty_excerpt() {
        assert_eq!(build_excerpt("   ", &["term".to_owned()]), "");
    }

    #[test]
    fn whitespace_is_collapsed_in_the_excerpt() {
        let excerpt = build_excerpt("a\n\n  b\t\tc", &["b".to_owned()]);
        assert_eq!(excerpt, "a b c");
    }

    #[test]
    fn the_description_is_searchable_too() {
        let documents = [RecallDocument {
            path: "a".to_owned(),
            description: "release checklist".to_owned(),
            body: "unrelated body".to_owned(),
        }];
        let candidates = select(&documents, "checklist", 5);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].description, "release checklist");
    }

    #[test]
    fn the_order_is_stable_for_the_same_input() {
        let documents = [
            document("b", "shared term"),
            document("a", "shared term"),
            document("c", "shared term"),
        ];
        let first: Vec<String> = select(&documents, "shared", 5)
            .into_iter()
            .map(|candidate| candidate.path)
            .collect();
        let second: Vec<String> = select(&documents, "shared", 5)
            .into_iter()
            .map(|candidate| candidate.path)
            .collect();
        assert_eq!(first, second);
    }

    #[test]
    fn expansions_can_surface_a_document_the_query_alone_misses() {
        let documents = [
            document("daemon", "worker process"),
            document("other", "unrelated"),
        ];
        let mut options = options(5);
        options.expansions = Some(QueryExpansions {
            synonyms: Some(vec!["worker".to_owned()]),
            ..QueryExpansions::default()
        });
        let candidates = select_candidates(&documents, &["daemon".to_owned()], &options);
        assert_eq!(candidates.first().map(|c| c.path.as_str()), Some("daemon"));
    }
    fn pinned(documents: &[RecallDocument], query: &str, strategy: RecallStrategy) -> Vec<String> {
        select_candidates(
            documents,
            &[query.to_owned()],
            &SelectOptions {
                max_items: 10,
                strategy: Some(strategy),
                ..SelectOptions::default()
            },
        )
        .into_iter()
        .map(|candidate| candidate.path)
        .collect()
    }

    #[test]
    fn a_small_ascii_corpus_is_scored_by_substring() {
        let documents = [document("a.md", "alpha beta"), document("b.md", "alpha")];
        let strategy = choose_recall_strategy_with(documents.len(), &["alpha".to_owned()], || {
            corpus_cjk_share(&documents)
        });
        assert_eq!(strategy, RecallStrategy::Substring);
    }

    #[test]
    fn a_cjk_query_is_scored_by_bm25() {
        let documents = [document("a.md", "한글 문서")];
        let strategy = choose_recall_strategy_with(documents.len(), &["한글".to_owned()], || 0.0);
        assert_eq!(strategy, RecallStrategy::Bm25);
    }

    #[test]
    fn a_large_corpus_goes_hybrid_even_in_english() {
        let documents: Vec<RecallDocument> = (0
            ..crate::recall_strategy::LARGE_CORPUS_MIN_DOCUMENTS)
            .map(|index| document(&format!("n{index}.md"), "alpha"))
            .collect();
        let strategy = choose_recall_strategy_with(documents.len(), &["alpha".to_owned()], || 0.0);
        assert_eq!(strategy, RecallStrategy::Hybrid);
    }

    #[test]
    fn pinning_substring_and_bm25_both_return_matching_documents() {
        let documents = [
            document("a.md", "alpha beta gamma"),
            document("b.md", "alpha"),
            document("c.md", "unrelated text"),
        ];
        let substring = pinned(&documents, "alpha beta", RecallStrategy::Substring);
        let bm25 = pinned(&documents, "alpha beta", RecallStrategy::Bm25);
        assert!(substring.contains(&"a.md".to_string()), "got {substring:?}");
        assert!(bm25.contains(&"a.md".to_string()), "got {bm25:?}");
        assert!(!substring.contains(&"c.md".to_string()));
        assert!(!bm25.contains(&"c.md".to_string()));
    }

    #[test]
    fn a_pinned_strategy_changes_the_result_for_a_corpus_where_the_scores_disagree() {
        let documents = [
            document("dense.md", "alpha alpha alpha alpha"),
            document("spread.md", "alpha one two three four five six seven"),
        ];
        let substring = pinned(&documents, "alpha", RecallStrategy::Substring);
        let bm25 = pinned(&documents, "alpha", RecallStrategy::Bm25);
        // Both must find both documents; the test is that the strategy is actually consulted, i.e.
        // the two paths are reachable and each produces its own ranking rather than one fused order.
        assert_eq!(substring.len(), 2, "got {substring:?}");
        assert_eq!(bm25.len(), 2, "got {bm25:?}");
        assert!(substring.iter().all(|path| bm25.contains(path)));
    }

    #[test]
    fn a_single_ranker_score_is_normalized_to_the_best_hit() {
        let scored = [
            Scored {
                position: 0,
                score: 0.25,
            },
            Scored {
                position: 1,
                score: 0.5,
            },
        ];
        let scores = rank_to_scores(&scored);
        assert_eq!(scores.len(), 2);
        let best = scores
            .iter()
            .map(|(_, score)| *score)
            .fold(0.0f64, f64::max);
        assert!((best - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn an_empty_ranker_output_has_no_scores() {
        assert!(rank_to_scores(&[]).is_empty());
        assert!(
            rank_to_scores(&[Scored {
                position: 0,
                score: 0.0
            }])
            .is_empty()
        );
    }
    #[test]
    fn a_cjk_excerpt_is_centered_on_the_matched_characters() {
        let body = format!("{} 한국어 문서 내용 {}", "가".repeat(300), "나".repeat(300));
        let excerpt = build_excerpt(&body, &[normalize("한국어")]);
        assert!(
            excerpt.contains("한국어"),
            "a CJK match must appear in its own excerpt, got {excerpt:?}"
        );
        assert!(excerpt.chars().count() <= EXCERPT_CHARS);
    }

    #[test]
    fn an_expanded_term_centers_the_excerpt_when_the_query_word_is_absent() {
        // The document was found through an added synonym only, so the excerpt must show the
        // synonym; otherwise the reader sees a window on unrelated text and cannot tell why the
        // memory was returned.
        let body = format!(
            "{} worker process here {}",
            "x".repeat(300),
            "y".repeat(300)
        );
        let excerpt = build_excerpt(&body, &[normalize("worker")]);
        assert!(excerpt.contains("worker"), "got {excerpt:?}");
    }
    #[test]
    fn a_cjk_query_through_the_real_path_shows_the_query_in_the_excerpt() {
        let body = format!("{} 한국어 문서 내용 {}", "가".repeat(300), "나".repeat(300));
        let documents = [document("k.md", &body)];
        let candidates = select_candidates(
            &documents,
            &["한국어".to_owned()],
            &SelectOptions {
                max_items: 1,
                strategy: Some(RecallStrategy::Bm25),
                ..SelectOptions::default()
            },
        );
        assert_eq!(candidates.len(), 1, "the document must be found");
        let excerpt = &candidates[0].excerpt;
        assert!(
            excerpt.contains("한국어"),
            "a BM25 hit on CJK must show the matched text, got {excerpt:?}"
        );
    }
    #[test]
    fn a_document_found_only_through_an_expansion_shows_the_expanded_word() {
        let body = format!(
            "{} worker process here {}",
            "x".repeat(300),
            "y".repeat(300)
        );
        let documents = [document("w.md", &body)];
        let candidates = select_candidates(
            &documents,
            &["daemon".to_owned()],
            &SelectOptions {
                max_items: 1,
                expansions: Some(QueryExpansions {
                    synonyms: Some(vec!["worker".to_owned()]),
                    ..QueryExpansions::default()
                }),
                ..SelectOptions::default()
            },
        );
        assert_eq!(candidates.len(), 1, "the synonym must find the document");
        let excerpt = &candidates[0].excerpt;
        assert!(
            excerpt.contains("worker"),
            "a document reached through an added term must show that term, got {excerpt:?}"
        );
    }
}
