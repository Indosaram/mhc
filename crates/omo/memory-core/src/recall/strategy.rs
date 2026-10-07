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
    let mut cjk = 0usize;
    let mut letters = 0usize;
    for document in documents {
        let text = normalized_haystack(document);
        for ch in text.chars() {
            if is_cjk_char(ch) {
                cjk += 1;
            }
            if is_letter(ch) {
                letters += 1;
            }
        }
    }
    if letters == 0 {
        0.0
    } else {
        cjk as f64 / letters as f64
    }
}

pub fn choose_recall_strategy(documents: &[RecallDocument], queries: &[String]) -> RecallStrategy {
    if documents.len() >= LARGE_CORPUS_MIN_DOCUMENTS {
        return RecallStrategy::Hybrid;
    }
    if queries.iter().any(|query| has_cjk(query))
        || corpus_cjk_share(documents) >= CJK_CORPUS_MIN_SHARE
    {
        return RecallStrategy::Bm25;
    }
    RecallStrategy::Substring
}

#[cfg(test)]
#[path = "strategy_tests.rs"]
mod tests;
