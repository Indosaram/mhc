//! How recall decides which scorer to run.
//!
//! Spec: `docs/cleanroom/specs/memory-recall-strategy.md`. Three scorers exist because the corpus differs by
//! installation: substring matching is exact and cheap, BM25 handles CJK where substring scoring degenerates,
//! and hybrid pays for both once the corpus is big enough that recall quality matters more than latency.

use crate::bm25::{RecallDocument, is_cjk_char};
use crate::transcript_search::normalize_text;

pub fn normalized_haystack(document: &RecallDocument) -> String {
    normalize_text(&format!("{}\n{}", document.description, document.body))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecallStrategy {
    Substring,
    Bm25,
    Hybrid,
}

/// Share of CJK code points at which the corpus counts as CJK.
///
/// Low on purpose: a corpus does not need to be mostly Chinese to break substring scoring, it only needs enough
/// CJK that exact substring matches stop being a useful signal.
pub const CJK_CORPUS_MIN_SHARE: f64 = 0.1;

pub const LARGE_CORPUS_MIN_DOCUMENTS: usize = 200;

pub fn has_cjk(text: &str) -> bool {
    text.chars().any(is_cjk_char)
}

fn is_letter(ch: char) -> bool {
    ch.is_alphabetic() || is_cjk_char(ch)
}

pub fn corpus_cjk_share(documents: &[RecallDocument]) -> f64 {
    cjk_share(
        documents
            .iter()
            .map(|document| count_cjk_letters(&normalized_haystack(document))),
    )
}

fn count_cjk_letters(text: &str) -> (usize, usize) {
    let mut cjk = 0usize;
    let mut letters = 0usize;
    for ch in text.chars() {
        if is_cjk_char(ch) {
            cjk += 1;
        }
        if is_letter(ch) {
            letters += 1;
        }
    }
    (cjk, letters)
}

fn cjk_share(counts: impl Iterator<Item = (usize, usize)>) -> f64 {
    let (cjk, letters) = counts.fold(
        (0usize, 0usize),
        |(cjk, letters), (more_cjk, more_letters)| (cjk + more_cjk, letters + more_letters),
    );
    if letters == 0 {
        // An empty corpus has no letters, and dividing by zero would make the threshold comparison meaningless.
        0.0
    } else {
        cjk as f64 / letters as f64
    }
}

/// Size wins over script: a large corpus goes hybrid even when it is all English.
pub fn choose_recall_strategy(documents: &[RecallDocument], queries: &[String]) -> RecallStrategy {
    choose_recall_strategy_with(documents.len(), queries, || corpus_cjk_share(documents))
}

/// [`choose_recall_strategy`] with the corpus CJK share computed lazily — the scan only runs when the size and
/// query checks have not already decided.
pub(crate) fn choose_recall_strategy_with(
    document_count: usize,
    queries: &[String],
    cjk_share: impl FnOnce() -> f64,
) -> RecallStrategy {
    if document_count >= LARGE_CORPUS_MIN_DOCUMENTS {
        return RecallStrategy::Hybrid;
    }
    if queries.iter().any(|query| has_cjk(query)) || cjk_share() >= CJK_CORPUS_MIN_SHARE {
        return RecallStrategy::Bm25;
    }
    RecallStrategy::Substring
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(description: &str, body: &str) -> RecallDocument {
        RecallDocument {
            path: "notes/a.md".to_owned(),
            description: description.to_owned(),
            body: body.to_owned(),
        }
    }

    fn queries(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn the_haystack_joins_description_and_body() {
        let haystack = normalized_haystack(&document("Deploy  Notes", "Nightly\nDEPLOY."));
        assert_eq!(haystack, "deploy notes nightly deploy.");
    }

    #[test]
    fn the_haystack_is_normalized() {
        let haystack = normalized_haystack(&document("  A  ", "  B  "));
        assert!(!haystack.contains("  "), "{haystack:?}");
        assert!(haystack.contains('a') && haystack.contains('b'));
    }

    #[test]
    fn cjk_is_detected() {
        assert!(has_cjk("한국어"));
        assert!(has_cjk("日本語"));
        assert!(!has_cjk("english"));
        assert!(!has_cjk(""));
    }

    #[test]
    fn an_empty_corpus_has_a_zero_share() {
        assert_eq!(corpus_cjk_share(&[]), 0.0);
    }

    #[test]
    fn a_corpus_without_letters_has_a_zero_share() {
        assert_eq!(corpus_cjk_share(&[document("", "123 456")]), 0.0);
    }

    #[test]
    fn a_cjk_corpus_reports_its_share() {
        let share = corpus_cjk_share(&[document("한국어", "한국어")]);
        assert!((share - 1.0).abs() < 1e-9, "{share}");
    }

    #[test]
    fn a_latin_corpus_reports_no_share() {
        assert_eq!(corpus_cjk_share(&[document("english", "words")]), 0.0);
    }

    #[test]
    fn a_mixed_corpus_reports_the_letter_ratio() {
        let share = corpus_cjk_share(&[document("한a", "b")]);
        // One CJK letter among three letters.
        assert!((share - (1.0 / 3.0)).abs() < 1e-9, "{share}");
    }

    #[test]
    fn a_small_english_corpus_uses_substring() {
        assert_eq!(
            choose_recall_strategy(&[document("english", "words")], &queries(&["deploy"])),
            RecallStrategy::Substring
        );
    }

    #[test]
    fn a_cjk_query_forces_bm25() {
        assert_eq!(
            choose_recall_strategy(&[document("english", "words")], &queries(&["한국어"])),
            RecallStrategy::Bm25
        );
    }

    #[test]
    fn one_cjk_query_among_many_forces_bm25() {
        assert_eq!(
            choose_recall_strategy(
                &[document("english", "words")],
                &queries(&["deploy", "notes", "日本語"])
            ),
            RecallStrategy::Bm25
        );
    }

    #[test]
    fn a_cjk_corpus_forces_bm25_without_a_cjk_query() {
        assert_eq!(
            choose_recall_strategy(&[document("한국어", "한국어")], &queries(&["deploy"])),
            RecallStrategy::Bm25
        );
    }

    #[test]
    fn a_large_corpus_uses_hybrid() {
        let documents = vec![document("english", "words"); LARGE_CORPUS_MIN_DOCUMENTS];
        assert_eq!(
            choose_recall_strategy(&documents, &queries(&["deploy"])),
            RecallStrategy::Hybrid
        );
    }

    #[test]
    fn a_large_cjk_corpus_still_uses_hybrid() {
        let documents = vec![document("한국어", "한국어"); LARGE_CORPUS_MIN_DOCUMENTS];
        assert_eq!(
            choose_recall_strategy(&documents, &queries(&["한국어"])),
            RecallStrategy::Hybrid
        );
    }

    #[test]
    fn one_note_below_the_size_threshold_is_not_large() {
        let documents = vec![document("english", "words"); LARGE_CORPUS_MIN_DOCUMENTS - 1];
        assert_eq!(
            choose_recall_strategy(&documents, &queries(&["deploy"])),
            RecallStrategy::Substring
        );
    }

    #[test]
    fn the_share_is_only_computed_when_it_can_change_the_answer() {
        let mut called = false;
        let strategy =
            choose_recall_strategy_with(LARGE_CORPUS_MIN_DOCUMENTS, &queries(&[]), || {
                called = true;
                0.0
            });
        assert_eq!(strategy, RecallStrategy::Hybrid);
        assert!(!called, "a large corpus must decide on size alone");
    }

    #[test]
    fn a_cjk_query_skips_the_share_scan() {
        let mut called = false;
        let strategy = choose_recall_strategy_with(1, &queries(&["日本語"]), || {
            called = true;
            0.0
        });
        assert_eq!(strategy, RecallStrategy::Bm25);
        assert!(!called, "a CJK query decides without scanning the corpus");
    }

    #[test]
    fn the_share_is_consulted_when_nothing_else_decided() {
        let mut called = false;
        let strategy = choose_recall_strategy_with(1, &queries(&["deploy"]), || {
            called = true;
            0.5
        });
        assert_eq!(strategy, RecallStrategy::Bm25);
        assert!(called);
    }
}
