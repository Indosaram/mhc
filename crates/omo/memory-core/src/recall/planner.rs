//! Recall query planner: newest-first conversation texts into short lexical queries
//! (pin `recall/planner.ts`).

use std::collections::BTreeSet;

/// Cap on the planned queries.
pub const MAX_RECALL_QUERIES: usize = 4;

const MAX_SINGLE_TERMS: usize = 2;
const MAX_TOOL_SINGLE_TERMS: usize = 2;
const MAX_PHRASES: usize = 2;
const MIN_ASCII_TERM_LENGTH: usize = 3;
const MIN_NON_ASCII_TERM_LENGTH: usize = 2;

const STOPWORDS: &[&str] = &[
    "about", "after", "again", "all", "also", "always", "and", "any", "are", "arent", "back", "been",
    "before", "being", "both", "but", "by", "can", "cant", "come", "could", "couldnt", "did", "didnt",
    "do", "does", "doesnt", "doing", "done", "down", "each", "even", "few", "for", "from", "get",
    "gets", "getting", "give", "go", "going", "gonna", "got", "had", "hadnt", "has", "hasnt", "have",
    "havent", "having", "her", "here", "hers", "him", "his", "how", "i", "if", "im", "into", "is",
    "isnt", "it", "its", "ive", "just", "lets", "like", "look", "looking", "made", "make", "may", "me",
    "might", "more", "most", "much", "must", "my", "need", "next", "no", "not", "now", "of", "off",
    "on", "once", "only", "or", "other", "our", "out", "over", "own", "per", "please", "same", "see",
    "shall", "she", "should", "shouldnt", "so", "some", "still", "such", "sure", "than", "that",
    "thats", "the", "their", "them", "then", "there", "these", "they", "theyre", "this", "those",
    "through", "too", "under", "until", "up", "us", "use", "used", "using", "very", "via", "want",
    "was", "wasnt", "we", "well", "were", "werent", "what", "whats", "when", "where", "which", "while",
    "who", "why", "will", "with", "without", "wont", "would", "yes", "yet", "you", "your", "youre",
    "youve",
];

const KOREAN_STOPWORDS: &[&str] = &[
    "거기", "거야", "그거", "그게", "그냥", "그러니까", "그러면", "그런데", "그래서", "그리고", "네",
    "누가", "뭐", "보자", "아니", "아니야", "어디", "어떻게", "왜", "응", "이거", "이게", "이제",
    "있어요", "저거", "저게", "저기", "정말", "좀", "진짜", "합니다", "하고", "했어", "했어요", "해줘",
    "해주세요",
];

const COMMAND_STOPWORDS: &[&str] = &[
    "awk", "bash", "bun", "bunx", "cat", "cd", "cp", "curl", "echo", "env", "eval", "export", "false",
    "find", "git", "grep", "head", "jq", "ls", "mkdir", "mv", "node", "npm", "npx", "pnpm", "printf",
    "read", "rg", "rm", "sed", "set", "sh", "sort", "tail", "tee", "timeout", "true", "uniq", "wc",
    "xargs", "zsh",
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

fn is_ascii_only(term: &str) -> bool {
    term.is_ascii()
}

fn is_kept_term(term: &str) -> bool {
    let min_length = if is_ascii_only(term) {
        MIN_ASCII_TERM_LENGTH
    } else {
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

fn ranked_terms(token_lists: &[Vec<String>]) -> Vec<TermRank> {
    let mut first_text_index: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let mut first_sequence: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
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
#[path = "planner_tests.rs"]
mod tests;
