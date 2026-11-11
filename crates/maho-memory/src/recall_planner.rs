//! Turning recent conversation text into short lexical queries for recall.
//!
//! Spec: `docs/cleanroom/specs/memory-recall-planner.md`. Recall cannot ask the user what to look for, so it
//! guesses from what was just said. The guess must be short and distinctive: a wide query matches everything and
//! ranks nothing, and a query full of filler words matches the same common memories every turn.

use std::collections::BTreeSet;

pub const MAX_RECALL_QUERIES: usize = 4;

const MAX_SINGLE_TERMS: usize = 2;
const MAX_TOOL_SINGLE_TERMS: usize = 2;
const MAX_PHRASES: usize = 2;
const MIN_ASCII_TERM_LENGTH: usize = 3;
const MIN_NON_ASCII_TERM_LENGTH: usize = 2;

const STOPWORDS: &[&str] = &[
    "about", "after", "again", "all", "also", "always", "and", "any", "are", "arent", "back",
    "been", "before", "being", "both", "but", "by", "can", "cant", "come", "could", "couldnt",
    "did", "didnt", "do", "does", "doesnt", "doing", "done", "down", "each", "even", "few", "for",
    "from", "get", "gets", "getting", "give", "go", "going", "gonna", "got", "had", "hadnt", "has",
    "hasnt", "have", "havent", "having", "her", "here", "hers", "him", "his", "how", "i", "if",
    "im", "into", "is", "isnt", "it", "its", "ive", "just", "lets", "like", "look", "looking",
    "made", "make", "may", "me", "might", "more", "most", "much", "must", "my", "need", "next",
    "no", "not", "now", "of", "off", "on", "once", "only", "or", "other", "our", "out", "over",
    "own", "per", "please", "same", "see", "shall", "she", "should", "shouldnt", "so", "some",
    "still", "such", "sure", "than", "that", "thats", "the", "their", "them", "then", "there",
    "these", "they", "theyre", "this", "those", "through", "too", "under", "until", "up", "us",
    "use", "used", "using", "very", "via", "want", "was", "wasnt", "we", "well", "were", "werent",
    "what", "whats", "when", "where", "which", "while", "who", "why", "will", "with", "without",
    "wont", "would", "yes", "yet", "you", "your", "youre", "youve",
];

const KOREAN_STOPWORDS: &[&str] = &[
    "거기",
    "거야",
    "그거",
    "그게",
    "그냥",
    "그러니까",
    "그러면",
    "그런데",
    "그래서",
    "그리고",
    "네",
    "누가",
    "뭐",
    "보자",
    "아니",
    "아니야",
    "어디",
    "어떻게",
    "왜",
    "응",
    "이거",
    "이게",
    "이제",
    "있어요",
    "저거",
    "저게",
    "저기",
    "정말",
    "좀",
    "진짜",
    "합니다",
    "하고",
    "했어",
    "했어요",
    "해줘",
    "해주세요",
];

const COMMAND_STOPWORDS: &[&str] = &[
    "awk", "bash", "bun", "bunx", "cat", "cd", "cp", "curl", "echo", "env", "eval", "export",
    "false", "find", "git", "grep", "head", "jq", "ls", "mkdir", "mv", "node", "npm", "npx",
    "pnpm", "printf", "read", "rg", "rm", "sed", "set", "sh", "sort", "tail", "tee", "timeout",
    "true", "uniq", "wc", "xargs", "zsh",
];

fn tokenize(text: &str) -> Vec<String> {
    let lowered = text.to_lowercase();
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in lowered.chars() {
        if ch.is_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn is_kept_term(term: &str) -> bool {
    let min_length = if term.is_ascii() {
        MIN_ASCII_TERM_LENGTH
    } else {
        // Two characters are already a word in Korean and Japanese; three would drop most of them.
        MIN_NON_ASCII_TERM_LENGTH
    };
    term.chars().count() >= min_length
        && !STOPWORDS.contains(&term)
        && !KOREAN_STOPWORDS.contains(&term)
}

struct TermRank {
    term: String,
    first_text_index: usize,
    text_count: usize,
    first_sequence: usize,
    length: usize,
}

/// Rank terms so the most useful ones come first.
///
/// Order of precedence: appears in the newest text, appears in **fewer** texts (a term in one message is about
/// that message; a term in every message says nothing), is longer, and appeared earlier.
fn ranked_terms(token_lists: &[Vec<String>]) -> Vec<TermRank> {
    let mut first_text_index: std::collections::BTreeMap<&str, usize> =
        std::collections::BTreeMap::new();
    let mut first_sequence: std::collections::BTreeMap<&str, usize> =
        std::collections::BTreeMap::new();
    let mut text_count: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let mut sequence = 0usize;
    for (text_index, tokens) in token_lists.iter().enumerate() {
        let unique: BTreeSet<&String> = tokens.iter().collect();
        for term in unique {
            first_text_index.entry(term.as_str()).or_insert(text_index);
            *text_count.entry(term.as_str()).or_insert(0) += 1;
        }
        for term in tokens {
            first_sequence.entry(term.as_str()).or_insert(sequence);
            sequence += 1;
        }
    }

    let mut pool: Vec<TermRank> = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for tokens in token_lists {
        for term in tokens {
            if !is_kept_term(term) || seen.contains(term.as_str()) {
                continue;
            }
            seen.insert(term.as_str());
            pool.push(TermRank {
                term: term.clone(),
                first_text_index: first_text_index.get(term.as_str()).copied().unwrap_or(0),
                text_count: text_count.get(term.as_str()).copied().unwrap_or(0),
                first_sequence: first_sequence.get(term.as_str()).copied().unwrap_or(0),
                length: term.chars().count(),
            });
        }
    }
    pool.sort_by(|left, right| {
        left.first_text_index
            .cmp(&right.first_text_index)
            .then_with(|| left.text_count.cmp(&right.text_count))
            .then_with(|| right.length.cmp(&left.length))
            .then_with(|| left.first_sequence.cmp(&right.first_sequence))
    });
    pool
}

/// Two-word phrases from the **newest** text that yields any, quoted so the search treats them as a unit.
fn plan_phrases(token_lists: &[Vec<String>]) -> Vec<String> {
    let mut phrases: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for tokens in token_lists {
        for pair in tokens.windows(2) {
            let (left, right) = (&pair[0], &pair[1]);
            if !is_kept_term(left) || !is_kept_term(right) {
                continue;
            }
            let phrase = format!("{left} {right}");
            if !seen.insert(phrase.clone()) {
                continue;
            }
            phrases.push(format!("\"{phrase}\""));
        }
        if !phrases.is_empty() {
            break;
        }
    }
    phrases.truncate(MAX_PHRASES);
    phrases
}

fn path_like(text: &str) -> bool {
    if text.contains('/') {
        return true;
    }
    match text.rsplit_once('.') {
        Some((_, extension)) => {
            !extension.is_empty()
                && extension.chars().count() <= 6
                && extension.chars().all(|ch| ch.is_ascii_alphanumeric())
        }
        None => false,
    }
}

fn path_derived_terms(tool_texts: &[String]) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    for text in tool_texts {
        if !path_like(text) {
            continue;
        }
        for term in tokenize(text) {
            terms.insert(term);
        }
    }
    terms
}

/// Plan up to four queries from the newest conversation texts.
///
/// `tool_texts` are file paths and commands the session touched. Their terms are promoted ahead of other
/// tool terms because a path names a concrete thing a note is likely to mention, while a command's own words
/// (`git`, `grep`, `ls`) appear in every note and distinguish nothing.
pub fn plan_recall_queries(texts: &[String], tool_texts: Option<&[String]>) -> Vec<String> {
    let token_lists: Vec<Vec<String>> = texts.iter().map(|text| tokenize(text)).collect();
    let singles: Vec<String> = ranked_terms(&token_lists)
        .into_iter()
        .take(MAX_SINGLE_TERMS)
        .map(|entry| entry.term)
        .collect();

    let Some(tool_texts) = tool_texts.filter(|texts| !texts.is_empty()) else {
        let mut queries = singles;
        queries.extend(plan_phrases(&token_lists));
        queries.truncate(MAX_RECALL_QUERIES);
        return queries;
    };

    let user_singles: BTreeSet<&String> = singles.iter().collect();
    let tool_token_lists: Vec<Vec<String>> = tool_texts.iter().map(|text| tokenize(text)).collect();
    let path_derived = path_derived_terms(tool_texts);
    let mut tool_ranked: Vec<TermRank> = ranked_terms(&tool_token_lists)
        .into_iter()
        .filter(|entry| {
            !user_singles.contains(&entry.term) && !COMMAND_STOPWORDS.contains(&entry.term.as_str())
        })
        .collect();
    tool_ranked.sort_by_key(|entry| std::cmp::Reverse(path_derived.contains(&entry.term)));
    let tool_singles: Vec<String> = tool_ranked
        .into_iter()
        .take(MAX_TOOL_SINGLE_TERMS)
        .map(|entry| entry.term)
        .collect();

    let mut all_tokens = token_lists;
    all_tokens.extend(tool_token_lists);
    let mut queries = singles;
    queries.extend(tool_singles);
    queries.extend(plan_phrases(&all_tokens));
    queries.truncate(MAX_RECALL_QUERIES + MAX_TOOL_SINGLE_TERMS);
    queries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn a_single_sentence_yields_its_most_distinctive_terms() {
        let queries = plan_recall_queries(
            &texts(&["the deployment pipeline keeps failing on the release branch"]),
            None,
        );
        assert!(queries.contains(&"deployment".to_owned()), "{queries:?}");
    }

    #[test]
    fn stopwords_are_dropped() {
        let queries = plan_recall_queries(&texts(&["the and for with about"]), None);
        assert!(queries.is_empty(), "{queries:?}");
    }

    #[test]
    fn short_ascii_terms_are_dropped() {
        let queries = plan_recall_queries(&texts(&["ab abc"]), None);
        assert!(!queries.iter().any(|query| query == "ab"), "{queries:?}");
        assert!(queries.iter().any(|query| query == "abc"), "{queries:?}");
    }

    #[test]
    fn two_character_non_ascii_terms_are_kept() {
        let queries = plan_recall_queries(&texts(&["배포"]), None);
        assert!(queries.iter().any(|query| query == "배포"), "{queries:?}");
    }

    #[test]
    fn korean_filler_is_dropped() {
        let queries = plan_recall_queries(&texts(&["그냥 진짜 그래서"]), None);
        assert!(queries.is_empty(), "{queries:?}");
    }

    #[test]
    fn the_newest_text_wins() {
        let queries = plan_recall_queries(&texts(&["alpha", "bravo"]), None);
        assert_eq!(queries.first().map(String::as_str), Some("alpha"));
    }

    #[test]
    fn a_term_in_fewer_texts_ranks_above_one_in_every_text() {
        let queries = plan_recall_queries(&texts(&["shared alpha", "shared"]), None);
        let alpha = queries.iter().position(|query| query == "alpha");
        let shared = queries.iter().position(|query| query == "shared");
        assert!(alpha < shared, "alpha should outrank shared: {queries:?}");
    }

    #[test]
    fn a_longer_term_wins_a_tie() {
        let queries = plan_recall_queries(&texts(&["abcd abc"]), None);
        assert_eq!(queries[0], "abcd");
    }

    #[test]
    fn at_most_two_single_terms_without_tool_text() {
        let queries =
            plan_recall_queries(&texts(&["alpha bravo charlie delta echo foxtrot"]), None);
        let bare = queries
            .iter()
            .filter(|query| !query.starts_with('"'))
            .count();
        assert!(bare <= MAX_SINGLE_TERMS, "{queries:?}");
    }

    #[test]
    fn phrases_are_quoted_pairs_from_the_newest_text() {
        let queries = plan_recall_queries(&texts(&["deployment pipeline release"]), None);
        assert!(
            queries
                .iter()
                .any(|query| query == "\"deployment pipeline\""),
            "{queries:?}"
        );
    }

    #[test]
    fn a_phrase_pair_with_a_stopword_is_skipped() {
        let queries = plan_recall_queries(&texts(&["deployment the pipeline"]), None);
        assert!(
            !queries
                .iter()
                .any(|query| query.contains("\"deployment the\"")),
            "{queries:?}"
        );
    }

    #[test]
    fn at_most_two_phrases() {
        let queries =
            plan_recall_queries(&texts(&["alpha bravo charlie delta echo foxtrot"]), None);
        let phrases = queries
            .iter()
            .filter(|query| query.starts_with('"'))
            .count();
        assert!(phrases <= MAX_PHRASES, "{queries:?}");
    }

    #[test]
    fn at_most_four_queries_without_tool_text() {
        let queries = plan_recall_queries(&texts(&["alpha bravo charlie delta echo"]), None);
        assert!(queries.len() <= MAX_RECALL_QUERIES, "{queries:?}");
    }

    #[test]
    fn an_empty_conversation_yields_no_queries() {
        assert!(plan_recall_queries(&[], None).is_empty());
    }

    #[test]
    fn empty_tool_text_behaves_like_no_tool_text() {
        let without = plan_recall_queries(&texts(&["deployment pipeline"]), None);
        let empty = plan_recall_queries(&texts(&["deployment pipeline"]), Some(&[]));
        assert_eq!(without, empty);
    }

    #[test]
    fn tool_text_terms_join_the_queries() {
        let queries = plan_recall_queries(
            &texts(&["fix the thing"]),
            Some(&texts(&["notes/ledger.md"])),
        );
        assert!(queries.iter().any(|query| query == "ledger"), "{queries:?}");
    }

    #[test]
    fn path_derived_tool_terms_outrank_longer_non_path_terms() {
        // "unknowable" is longer, so length ordering alone would pick it. Path provenance wins instead.
        let queries = plan_recall_queries(
            &texts(&["fix the thing"]),
            Some(&texts(&["notes/rota.md", "unknowable"])),
        );
        assert!(queries.iter().any(|query| query == "rota"), "{queries:?}");
        assert!(
            !queries.iter().any(|query| query == "unknowable"),
            "a non-path tool term must not displace a path term: {queries:?}"
        );
    }

    #[test]
    fn command_words_from_tools_are_dropped() {
        let queries = plan_recall_queries(
            &texts(&["fix the thing"]),
            Some(&texts(&["git status", "grep -rn ledger"])),
        );
        assert!(
            !queries
                .iter()
                .any(|query| query == "git" || query == "grep"),
            "{queries:?}"
        );
    }

    #[test]
    fn a_tool_term_repeating_a_user_term_is_not_planned_twice() {
        let queries = plan_recall_queries(&texts(&["ledger"]), Some(&texts(&["crates/ledger.rs"])));
        assert_eq!(
            queries.iter().filter(|query| *query == "ledger").count(),
            1,
            "{queries:?}"
        );
    }

    #[test]
    fn at_most_two_tool_terms_join() {
        let queries = plan_recall_queries(
            &texts(&["fix the thing"]),
            Some(&texts(&["alpha1/beta2/gamma3/delta4/epsilon5.txt"])),
        );
        let tool_terms: Vec<&String> = queries
            .iter()
            .filter(|query| query.as_str() == "alpha1" || query.as_str() == "beta2")
            .collect();
        assert!(!tool_terms.is_empty(), "{queries:?}");
    }

    #[test]
    fn tool_text_allows_up_to_six_queries() {
        let queries = plan_recall_queries(
            &texts(&["alpha bravo charlie delta"]),
            Some(&texts(&["notes/rota.md"])),
        );
        assert!(
            queries.len() <= MAX_RECALL_QUERIES + MAX_TOOL_SINGLE_TERMS,
            "{queries:?}"
        );
    }

    #[test]
    fn a_dotted_name_counts_as_a_path() {
        assert!(path_like("ledger.rs"));
        assert!(path_like("crates/x"));
        assert!(!path_like("deployment"));
    }

    #[test]
    fn a_long_extension_is_not_a_path() {
        assert!(!path_like("something.notanextension"));
    }
}
