//! Light English suffix folding for the recall BM25 index.
//!
//! Spec: `docs/cleanroom/specs/memory-english-stem.md`. Folding is applied at index and query time
//! alike, so "memories" and "memory" land on the same term without a dictionary.

const MIN_FOLDED_LENGTH: usize = 3;
const MIN_STEM_LENGTH: usize = 3;

fn is_ascii_word(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_lowercase())
}

fn has_vowel(word: &str) -> bool {
    word.chars()
        .any(|ch| matches!(ch, 'a' | 'e' | 'i' | 'o' | 'u' | 'y'))
}

/// A doubled final consonant left by a stripped suffix; `l`, `s` and `z` stay doubled.
fn has_doubled_consonant(word: &str) -> bool {
    let chars: Vec<char> = word.chars().collect();
    if chars.len() < 2 {
        return false;
    }
    let last = chars[chars.len() - 1];
    chars[chars.len() - 2] == last
        && !matches!(last, 'a' | 'e' | 'i' | 'o' | 'u' | 'y' | 'l' | 's' | 'z')
}

fn strip_plural(word: &str) -> String {
    if word.ends_with("sses") {
        return word[..word.len() - 2].to_string();
    }
    if word.ends_with("ies") && word.len() > 4 {
        return format!("{}y", &word[..word.len() - 3]);
    }
    if ends_with_sibilant_es(word) {
        return word[..word.len() - 2].to_string();
    }
    if word.ends_with('s')
        && !word.ends_with("ss")
        && !word.ends_with("us")
        && !word.ends_with("is")
    {
        return word[..word.len() - 1].to_string();
    }
    word.to_string()
}

fn ends_with_sibilant_es(word: &str) -> bool {
    let Some(body) = word.strip_suffix("es") else {
        return false;
    };
    body.ends_with("ch")
        || body.ends_with("sh")
        || body.ends_with('x')
        || body.ends_with('z')
        || body.ends_with("ss")
}

fn strip_suffix(word: &str, suffix: &str) -> Option<String> {
    let stem = word.strip_suffix(suffix)?;
    if stem.len() < MIN_STEM_LENGTH || !has_vowel(stem) {
        return None;
    }
    if has_doubled_consonant(stem) {
        return Some(stem[..stem.len() - 1].to_string());
    }
    Some(stem.to_string())
}

fn strip_derivational(word: &str) -> String {
    if word.ends_with("ment") && word.len() >= 8 {
        return word[..word.len() - 4].to_string();
    }
    if let Some(adverb) = strip_suffix(word, "ly") {
        return adverb;
    }
    if word.len() >= 6 && (word.ends_with("tion") || word.ends_with("sion")) {
        return word[..word.len() - 3].to_string();
    }
    word.to_string()
}

pub fn stem_english_token(token: &str) -> String {
    if token.chars().count() <= MIN_FOLDED_LENGTH || !is_ascii_word(token) {
        return token.to_string();
    }
    let mut word = strip_plural(token);
    word = strip_suffix(&word, "ing")
        .or_else(|| strip_suffix(&word, "ed"))
        .unwrap_or(word);
    word = strip_derivational(&word);
    if word.chars().count() > MIN_FOLDED_LENGTH && word.ends_with('e') {
        return word[..word.len() - 1].to_string();
    }
    word
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_tokens_are_left_alone() {
        assert_eq!(stem_english_token("is"), "is");
        assert_eq!(stem_english_token("the"), "the");
    }

    #[test]
    fn non_lowercase_ascii_is_left_alone() {
        assert_eq!(stem_english_token("Memory"), "Memory");
        assert_eq!(stem_english_token("memo-ry"), "memo-ry");
        assert_eq!(stem_english_token("\u{d55c}\u{ae00}"), "\u{d55c}\u{ae00}");
    }

    #[test]
    fn a_plain_plural_folds() {
        assert_eq!(stem_english_token("memories"), "memory");
        assert_eq!(stem_english_token("notes"), "not");
    }

    #[test]
    fn an_ss_ending_is_not_a_plural() {
        assert_eq!(stem_english_token("less"), "less");
        assert_eq!(stem_english_token("progress"), "progress");
    }

    #[test]
    fn a_us_or_is_ending_is_not_a_plural() {
        assert_eq!(stem_english_token("status"), "status");
        assert_eq!(stem_english_token("analysis"), "analysis");
    }

    #[test]
    fn a_sibilant_es_ending_folds() {
        assert_eq!(stem_english_token("batches"), "batch");
        assert_eq!(stem_english_token("boxes"), "box");
    }

    #[test]
    fn sses_collapses_to_ss() {
        assert_eq!(stem_english_token("classes"), "class");
    }

    #[test]
    fn a_short_ies_word_only_loses_its_s() {
        assert_eq!(stem_english_token("ties"), "tie");
    }

    #[test]
    fn an_ing_suffix_folds() {
        assert_eq!(stem_english_token("recording"), "record");
    }

    #[test]
    fn an_ed_suffix_folds() {
        assert_eq!(stem_english_token("recorded"), "record");
    }

    #[test]
    fn a_doubled_consonant_collapses() {
        assert_eq!(stem_english_token("running"), "run");
        assert_eq!(stem_english_token("stopped"), "stop");
    }

    #[test]
    fn l_s_and_z_stay_doubled() {
        assert_eq!(stem_english_token("calling"), "call");
        assert_eq!(stem_english_token("passing"), "pass");
    }

    #[test]
    fn a_stem_without_a_vowel_keeps_its_suffix() {
        assert_eq!(stem_english_token("bring"), "bring");
    }

    #[test]
    fn a_too_short_stem_keeps_its_suffix() {
        assert_eq!(stem_english_token("being"), "being");
    }

    #[test]
    fn a_ly_suffix_folds() {
        assert_eq!(stem_english_token("quickly"), "quick");
    }

    #[test]
    fn a_ment_suffix_folds() {
        assert_eq!(stem_english_token("agreement"), "agre");
    }

    #[test]
    fn a_tion_suffix_folds() {
        assert_eq!(stem_english_token("reflection"), "reflect");
        assert_eq!(stem_english_token("decision"), "decis");
    }

    #[test]
    fn a_trailing_e_drops() {
        assert_eq!(stem_english_token("archive"), "archiv");
    }

    #[test]
    fn a_short_word_ending_in_e_keeps_it() {
        assert_eq!(stem_english_token("code"), "cod");
        assert_eq!(stem_english_token("see"), "see");
    }

    #[test]
    fn folding_is_idempotent_for_a_folded_word() {
        let once = stem_english_token("memories");
        assert_eq!(stem_english_token(&once), once);
    }

    #[test]
    fn a_word_and_its_plural_fold_together() {
        assert_eq!(stem_english_token("memory"), stem_english_token("memories"));
        assert_eq!(stem_english_token("note"), stem_english_token("notes"));
    }
}
