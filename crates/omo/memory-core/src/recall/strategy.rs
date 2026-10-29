//! Recall strategy selection (pin `recall/strategy.ts`).

use super::bm25::is_cjk_char;
use super::haystack::normalized_haystack;
use super::provider::RecallDocument;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecallStrategy {
    Substring,
    Bm25,
    Hybrid,
}

/// Share of CJK code points among all letter code points at which the corpus counts as CJK.
pub const CJK_CORPUS_MIN_SHARE: f64 = 0.1;
/// Note count at which the corpus counts as large.
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

/// [`corpus_cjk_share`] over haystacks the caller already normalized with `normalized_haystack`.
pub(crate) fn haystack_cjk_share(haystacks: &[String]) -> f64 {
    cjk_share(
        haystacks
            .iter()
            .map(|haystack| count_cjk_letters(haystack.as_str())),
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
        0.0
    } else {
        cjk as f64 / letters as f64
    }
}

pub fn choose_recall_strategy(documents: &[RecallDocument], queries: &[String]) -> RecallStrategy {
    choose_recall_strategy_with(documents.len(), queries, || corpus_cjk_share(documents))
}

/// [`choose_recall_strategy`] with the corpus CJK share supplied lazily by the caller.
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
#[path = "strategy_tests.rs"]
mod tests;
