use super::{RankedRecallDocument, RecallQueryExpansions, recall_expansion_weights, recall_terms, rank_recall_documents_bm25, tokenize_recall_text};
use crate::recall::provider::RecallDocument;
use std::collections::BTreeSet;

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

fn paths(ranked: &[RankedRecallDocument]) -> Vec<String> {
    ranked.iter().map(|entry| entry.document.path.clone()).collect()
}

#[test]
fn given_cjk_runs_when_tokenized_then_they_expand_into_bigrams_and_han_characters() {
    assert_eq!(tokenize_recall_text("배포절차"), ["배포절차", "배포", "포절", "절차"]);
    assert_eq!(
        tokenize_recall_text("記憶検索 メモリ"),
        ["記憶検索", "記憶", "憶検", "検索", "記", "憶", "検", "索", "メモリ", "メモ", "モリ"]
    );
}

#[test]
fn given_latin_text_when_tokenized_then_it_is_lowercased_and_kept_as_whole_words() {
    let tokens = tokenize_recall_text("Rollback The Canary");
    assert_eq!(
        tokens,
        vec!["rollback".to_string(), "the".to_string(), "canary".to_string()]
    );
}

#[test]
fn given_inflected_words_when_terms_are_built_then_english_suffixes_are_folded() {
    let terms = recall_terms("deploying deployed deployment");
    assert!(terms.contains(&"deploy".to_string()));
}

#[test]
fn given_documents_sharing_a_query_term_when_ranked_then_only_matches_return_best_first() {
    let documents = [
        doc("a.md", "alpha", "alpha body"),
        doc("b.md", "beta", "beta body"),
        doc("c.md", "alpha", "alpha body"),
    ];
    let ranked = rank_recall_documents_bm25(&documents, &queries(&["alpha"]), None);
    assert_eq!(paths(&ranked), vec!["a.md".to_string(), "c.md".to_string()]);
}

#[test]
fn given_an_empty_query_when_ranked_then_nothing_returns() {
    let documents = [doc("a.md", "alpha", "alpha body")];
    assert!(rank_recall_documents_bm25(&documents, &[], None).is_empty());
}

#[test]
fn given_expansions_when_weighted_then_query_terms_are_left_out_and_tiers_keep_their_weight() {
    let expansions = RecallQueryExpansions {
        synonyms: Some(vec!["beta".to_string(), "alpha".to_string()]),
        keywords: Some(vec!["beta".to_string(), "gamma".to_string()]),
        related: None,
        note_line: None,
    };
    let query_terms: BTreeSet<String> = ["alpha".to_string()].into_iter().collect();
    let weights = recall_expansion_weights(Some(&expansions), &query_terms);
    assert_eq!(weights.get("beta"), Some(&0.75));
    assert_eq!(weights.get("gamma"), Some(&0.75));
    assert!(!weights.contains_key("alpha"));
}

#[test]
fn given_no_expansions_when_weighted_then_the_map_is_empty() {
    assert!(recall_expansion_weights(None, &BTreeSet::new()).is_empty());
}

#[test]
fn given_a_document_holding_every_query_unit_when_widened_then_it_is_lifted_to_the_front() {
    let documents = [
        doc("a.md", "noise", "noise noise noise"),
        doc("b.md", "alpha beta", "alpha beta together"),
    ];
    let expansions = RecallQueryExpansions {
        synonyms: Some(vec!["beta".to_string()]),
        keywords: None,
        related: None,
        note_line: None,
    };
    let ranked = rank_recall_documents_bm25(
        &documents,
        &queries(&["alpha"]),
        Some(&expansions),
    );
    assert_eq!(ranked.first().map(|entry| entry.document.path.clone()), Some("b.md".to_string()));
    assert!(ranked.first().is_some_and(|entry| entry.full_match));
}
