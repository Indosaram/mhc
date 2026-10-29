use super::{
    CJK_CORPUS_MIN_SHARE, LARGE_CORPUS_MIN_DOCUMENTS, RecallStrategy, choose_recall_strategy,
    corpus_cjk_share, has_cjk,
};
use crate::recall::provider::RecallDocument;

fn doc(path: &str, description: &str, body: &str) -> RecallDocument {
    RecallDocument {
        path: path.to_string(),
        description: description.to_string(),
        body: body.to_string(),
    }
}

fn english_corpus(count: usize) -> Vec<RecallDocument> {
    (0..count)
        .map(|index| doc(&format!("notes/n{index}.md"), "Deploy notes", "rollback the canary"))
        .collect()
}

#[test]
fn given_hangul_han_hiragana_katakana_and_the_prolonged_sound_mark_when_checked_then_each_counts_as_cjk()
{
    for sample in ["배포", "发布", "ひらがな", "カタカナ", "\u{30fc}"] {
        assert!(has_cjk(sample), "{sample} should count as CJK");
    }
}

#[test]
fn given_latin_cyrillic_digits_and_fullwidth_punctuation_when_checked_then_none_counts_as_cjk() {
    for sample in ["rollback", "откат", "2026", "。、", ""] {
        assert!(!has_cjk(sample), "{sample} should not count as CJK");
    }
}

#[test]
fn given_an_english_only_corpus_when_measured_then_the_share_is_zero() {
    assert_eq!(corpus_cjk_share(&english_corpus(3)), 0.0);
}

#[test]
fn given_mixed_letters_when_measured_then_the_share_counts_cjk_over_all_letter_code_points() {
    let documents = [doc("a.md", "ab", "배포 42 !")];
    assert_eq!(corpus_cjk_share(&documents), 0.5);
}

#[test]
fn given_an_empty_corpus_when_measured_then_the_share_is_zero() {
    assert_eq!(corpus_cjk_share(&[]), 0.0);
}

#[test]
fn given_english_queries_over_a_small_english_corpus_when_chosen_then_the_substring_path_is_kept() {
    assert_eq!(
        choose_recall_strategy(&english_corpus(10), &["rollback".to_string(), "\"canary deploy\"".to_string()]),
        RecallStrategy::Substring
    );
}

#[test]
fn given_one_planned_query_with_a_cjk_character_over_an_english_corpus_when_chosen_then_bm25_is_picked()
{
    assert_eq!(
        choose_recall_strategy(&english_corpus(10), &["rollback".to_string(), "퍼블리시할".to_string()]),
        RecallStrategy::Bm25
    );
}

#[test]
fn given_a_corpus_exactly_at_the_cjk_share_threshold_when_chosen_then_bm25_is_picked() {
    let latin = (1.0 / CJK_CORPUS_MIN_SHARE).round() as usize - 1;
    let documents = [doc("a.md", "", &format!("배{}", "a".repeat(latin)))];
    assert!((corpus_cjk_share(&documents) - CJK_CORPUS_MIN_SHARE).abs() < 1e-10);
    assert_eq!(
        choose_recall_strategy(&documents, &["rollback".to_string()]),
        RecallStrategy::Bm25
    );
}

#[test]
fn given_a_corpus_just_below_the_cjk_share_threshold_when_chosen_then_substring_is_kept() {
    let latin = (1.0 / CJK_CORPUS_MIN_SHARE).round() as usize;
    let documents = [doc("a.md", "", &format!("배{}", "a".repeat(latin)))];
    assert!(corpus_cjk_share(&documents) < CJK_CORPUS_MIN_SHARE);
    assert_eq!(
        choose_recall_strategy(&documents, &["rollback".to_string()]),
        RecallStrategy::Substring
    );
}

#[test]
fn given_an_english_corpus_one_note_below_the_large_threshold_when_chosen_then_substring_is_kept() {
    assert_eq!(
        choose_recall_strategy(
            &english_corpus(LARGE_CORPUS_MIN_DOCUMENTS - 1),
            &["rollback".to_string()]
        ),
        RecallStrategy::Substring
    );
}

#[test]
fn given_an_english_corpus_exactly_at_the_large_threshold_when_chosen_then_hybrid_is_picked() {
    assert_eq!(
        choose_recall_strategy(&english_corpus(LARGE_CORPUS_MIN_DOCUMENTS), &["rollback".to_string()]),
        RecallStrategy::Hybrid
    );
}

#[test]
fn given_a_cjk_query_over_a_large_corpus_when_chosen_then_hybrid_is_picked() {
    assert_eq!(
        choose_recall_strategy(&english_corpus(LARGE_CORPUS_MIN_DOCUMENTS), &["퍼블리시할".to_string()]),
        RecallStrategy::Hybrid
    );
}
