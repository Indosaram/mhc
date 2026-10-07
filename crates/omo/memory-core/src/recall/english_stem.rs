//! Light English suffix folding for the recall BM25 index (pin `recall/english-stem.ts`).

const MIN_FOLDED_LENGTH: usize = 3;
const MIN_STEM_LENGTH: usize = 3;

fn is_ascii_word(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_lowercase())
}

fn has_vowel(word: &str) -> bool {
    word.chars().any(|ch| matches!(ch, 'a' | 'e' | 'i' | 'o' | 'u' | 'y'))
}

/// A doubled final consonant left by a stripped suffix; l, s and z stay doubled.
fn has_doubled_consonant(word: &str) -> bool {
    let chars: Vec<char> = word.chars().collect();
    if chars.len() < 2 {
        return false;
    }
    let last = chars[chars.len() - 1];
    if chars[chars.len() - 2] != last || matches!(last, 'a' | 'e' | 'i' | 'o' | 'u' | 'y' | 'l' | 's' | 'z') {
        return false;
    }
    true
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
    if word.ends_with('s') && !word.ends_with("ss") && !word.ends_with("us") && !word.ends_with("is") {
        return word[..word.len() - 1].to_string();
    }
    word.to_string()
}

fn ends_with_sibilant_es(word: &str) -> bool {
    let body = match word.strip_suffix("es") {
        Some(body) => body,
        None => return false,
    };
    body.ends_with("ch") || body.ends_with("sh") || body.ends_with('x') || body.ends_with('z') || body.ends_with("ss")
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
    if word.ends_with("ment") && word.len() >= 4 + 4 {
        return word[..word.len() - 4].to_string();
    }
    if let Some(adverb) = strip_suffix(word, "ly") {
        return adverb;
    }
    if word.len() >= 3 + 3 && (word.ends_with("tion") || word.ends_with("sion")) {
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
#[path = "english_stem_tests.rs"]
mod tests;
