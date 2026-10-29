use std::collections::BTreeSet;

use super::{RecallCandidate, SelectRecallOptions, select_recall_candidates};
use crate::recall::provider::RecallDocument;
use crate::recall::strategy::RecallStrategy;

fn doc(path: &str, description: &str, body: &str) -> RecallDocument {
    RecallDocument {
        path: path.to_string(),
        description: description.to_string(),
        body: body.to_string(),
    }
}

fn queries(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn options(max_items: usize, strategy: RecallStrategy) -> SelectRecallOptions {
    SelectRecallOptions {
        max_items,
        surfaced: BTreeSet::new(),
        exclude_paths: None,
        strategy: Some(strategy),
        expansions: None,
    }
}

fn paths(candidates: &[RecallCandidate]) -> Vec<String> {
    candidates
        .iter()
        .map(|candidate| candidate.path.clone())
        .collect()
}

fn phrase_corpus() -> (Vec<RecallDocument>, Vec<String>) {
    let phrase = doc(
        "people/dana.md",
        "Dana",
        "Dana wants weekly status updates on Friday with the release date first.",
    );
    let noise = [
        doc("archive/a.md", "updates written", "updates written updates written"),
        doc("archive/b.md", "written updates", "written updates written"),
    ];
    let documents = vec![phrase, noise[0].clone(), noise[1].clone()];
    let queries = queries(&["updates", "written", "\"status updates\""]);
    (documents, queries)
}

#[test]
fn given_a_phrase_match_bm25_ranks_below_lone_word_matches_when_hybrid_selects_then_the_phrase_stays_first()
{
    let (documents, queries) = phrase_corpus();
    let bm25 = select_recall_candidates(
        &documents,
        &queries,
        &options(5, RecallStrategy::Bm25),
    );
    let hybrid = select_recall_candidates(
        &documents,
        &queries,
        &options(5, RecallStrategy::Hybrid),
    );

    assert_ne!(paths(&bm25).first(), Some(&"people/dana.md".to_string()));
    assert_eq!(paths(&hybrid).first(), Some(&"people/dana.md".to_string()));
    let hybrid_paths: BTreeSet<String> = paths(&hybrid).into_iter().collect();
    let document_paths: BTreeSet<String> = documents
        .iter()
        .map(|document| document.path.clone())
        .collect();
    assert_eq!(hybrid_paths, document_paths);
}

#[test]
fn given_the_pinned_phrase_match_when_scores_are_read_then_they_stay_ascending() {
    let (documents, queries) = phrase_corpus();
    let hybrid = select_recall_candidates(
        &documents,
        &queries,
        &options(5, RecallStrategy::Hybrid),
    );
    let scores: Vec<f64> = hybrid.iter().map(|candidate| candidate.score).collect();
    let mut sorted = scores.clone();
    sorted.sort_by(|left, right| left.partial_cmp(right).expect("comparable scores"));
    assert_eq!(scores, sorted);
}

#[test]
fn given_the_phrase_match_is_surfaced_when_hybrid_selects_then_the_floor_does_not_bring_it_back() {
    let (documents, queries) = phrase_corpus();
    let mut select_options = options(5, RecallStrategy::Hybrid);
    select_options.surfaced = BTreeSet::from(["people/dana.md".to_string()]);
    let hybrid = select_recall_candidates(&documents, &queries, &select_options);

    assert!(!paths(&hybrid).contains(&"people/dana.md".to_string()));
    assert_eq!(hybrid.len(), 2);
}

#[test]
fn given_only_lone_word_queries_when_hybrid_selects_then_fusion_alone_orders_the_notes() {
    let (documents, _) = phrase_corpus();
    let hybrid = select_recall_candidates(
        &documents,
        &queries(&["updates", "written"]),
        &options(5, RecallStrategy::Hybrid),
    );
    assert_ne!(paths(&hybrid).first(), Some(&"people/dana.md".to_string()));
}

#[test]
fn given_a_korean_phrase_match_when_hybrid_selects_then_fusion_orders_the_notes_instead_of_the_floor()
{
    let korean = [
        doc("reference/ko/early.md", "배포 절차", "배포 절차 문서"),
        doc("reference/ko/rich.md", "절차", "배포할 때 절차를 지키고 배포하면 절차 확인"),
    ];
    let korean_queries = queries(&["배포할", "절차를", "\"배포 절차\""]);
    let bm25 = select_recall_candidates(
        &korean,
        &korean_queries,
        &options(5, RecallStrategy::Bm25),
    );
    let hybrid = select_recall_candidates(
        &korean,
        &korean_queries,
        &options(5, RecallStrategy::Hybrid),
    );

    assert_eq!(paths(&bm25).first(), Some(&"reference/ko/rich.md".to_string()));
    assert_eq!(paths(&hybrid).first(), Some(&"reference/ko/rich.md".to_string()));
}

#[test]
fn given_no_queries_when_selecting_then_nothing_returns() {
    let documents = [doc("a.md", "alpha", "alpha body")];
    assert!(select_recall_candidates(&documents, &[], &options(5, RecallStrategy::Substring)).is_empty());
}

#[test]
fn given_zero_max_items_when_selecting_then_nothing_returns() {
    let documents = [doc("a.md", "alpha", "alpha body")];
    assert!(
        select_recall_candidates(
            &documents,
            &queries(&["alpha"]),
            &options(0, RecallStrategy::Substring)
        )
        .is_empty()
    );
}

#[test]
fn given_a_small_english_corpus_when_substring_selects_then_the_matching_note_is_found() {
    let documents = [
        doc("notes/a.md", "deploy", "rollback the canary before the release"),
        doc("notes/b.md", "unrelated", "nothing to see here"),
    ];
    let selected = select_recall_candidates(
        &documents,
        &queries(&["canary"]),
        &options(5, RecallStrategy::Substring),
    );
    assert_eq!(paths(&selected), vec!["notes/a.md".to_string()]);
    assert!(!selected[0].excerpt.is_empty());
}

#[test]
fn given_a_surfaced_path_when_substring_selects_then_it_is_excluded() {
    let documents = [doc("notes/a.md", "deploy", "rollback the canary")];
    let mut select_options = options(5, RecallStrategy::Substring);
    select_options.surfaced = BTreeSet::from(["notes/a.md".to_string()]);
    assert!(select_recall_candidates(&documents, &queries(&["canary"]), &select_options).is_empty());
}

#[test]
fn given_an_excluded_path_when_selecting_then_it_is_skipped() {
    let documents = [
        doc("notes/a.md", "deploy", "rollback the canary"),
        doc("notes/b.md", "deploy", "rollback the canary"),
    ];
    let mut select_options = options(5, RecallStrategy::Substring);
    select_options.exclude_paths = Some(BTreeSet::from(["notes/a.md".to_string()]));
    let selected = select_recall_candidates(&documents, &queries(&["canary"]), &select_options);
    assert_eq!(paths(&selected), vec!["notes/b.md".to_string()]);
}

#[test]
fn given_the_cap_when_selecting_then_only_that_many_candidates_return() {
    let documents = [
        doc("notes/a.md", "deploy", "rollback the canary"),
        doc("notes/b.md", "deploy", "rollback the canary"),
        doc("notes/c.md", "deploy", "rollback the canary"),
    ];
    let selected = select_recall_candidates(
        &documents,
        &queries(&["canary"]),
        &options(2, RecallStrategy::Substring),
    );
    assert_eq!(selected.len(), 2);
}
