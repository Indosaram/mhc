//! Port of components/session-selector-search.ts.
//!
//! senpi's `SessionInfo` comes from `core/session-manager.ts` (plan todo 21). This module declares
//! the exact field subset the search reads so the ranking logic can be ported and tested now; the
//! todo-35 mode converts its session records into `SessionInfo`.

use maho_tui::fuzzy::fuzzy_match;
use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub path: String,
    pub id: String,
    /// Working directory where the session was started. Empty string for old sessions.
    pub cwd: String,
    /// User-defined display name from session_info entries.
    pub name: Option<String>,
    /// Path to the parent session (if this session was forked).
    pub parent_session_path: Option<String>,
    /// Epoch milliseconds; senpi stores `Date` and compares by `getTime()`.
    pub modified_ms: i64,
    pub message_count: usize,
    pub first_message: String,
    pub all_messages_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    Threaded,
    Recent,
    Relevance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameFilter {
    All,
    Named,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Fuzzy,
    Phrase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchToken {
    pub kind: TokenKind,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Tokens,
    Regex,
}

#[derive(Debug, Clone)]
pub struct ParsedSearchQuery {
    pub mode: SearchMode,
    pub tokens: Vec<SearchToken>,
    pub regex: Option<Regex>,
    /// If set, parsing failed and the query should be treated as non-matching.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchResult {
    pub matches: bool,
    /// Lower is better; only meaningful when `matches` is true.
    pub score: f64,
}

fn normalize_whitespace_lower(text: &str) -> String {
    text.split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn get_session_search_text(session: &SessionInfo) -> String {
    format!(
        "{} {} {} {}",
        session.id,
        session.name.clone().unwrap_or_default(),
        session.all_messages_text,
        session.cwd
    )
}

pub fn has_session_name(session: &SessionInfo) -> bool {
    session.name.as_deref().is_some_and(|name| !name.trim().is_empty())
}

fn matches_name_filter(session: &SessionInfo, filter: NameFilter) -> bool {
    match filter {
        NameFilter::All => true,
        NameFilter::Named => has_session_name(session),
    }
}

pub fn parse_search_query(query: &str) -> ParsedSearchQuery {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return ParsedSearchQuery {
            mode: SearchMode::Tokens,
            tokens: Vec::new(),
            regex: None,
            error: None,
        };
    }

    // Regex mode: re:<pattern>
    if let Some(rest) = trimmed.strip_prefix("re:") {
        let pattern = rest.trim();
        if pattern.is_empty() {
            return ParsedSearchQuery {
                mode: SearchMode::Regex,
                tokens: Vec::new(),
                regex: None,
                error: Some("Empty regex".to_owned()),
            };
        }
        return match Regex::new(&format!("(?i){pattern}")) {
            Ok(regex) => ParsedSearchQuery {
                mode: SearchMode::Regex,
                tokens: Vec::new(),
                regex: Some(regex),
                error: None,
            },
            Err(error) => ParsedSearchQuery {
                mode: SearchMode::Regex,
                tokens: Vec::new(),
                regex: None,
                error: Some(error.to_string()),
            },
        };
    }

    // Token mode with quote support. Example: foo "node cve" bar
    let mut tokens: Vec<SearchToken> = Vec::new();
    let mut buf = String::new();
    let mut in_quote = false;

    fn flush(buf: &mut String, tokens: &mut Vec<SearchToken>, kind: TokenKind) {
        let value = buf.trim().to_owned();
        buf.clear();
        if !value.is_empty() {
            tokens.push(SearchToken { kind, value });
        }
    }

    for ch in trimmed.chars() {
        if ch == '"' {
            if in_quote {
                flush(&mut buf, &mut tokens, TokenKind::Phrase);
                in_quote = false;
            } else {
                flush(&mut buf, &mut tokens, TokenKind::Fuzzy);
                in_quote = true;
            }
            continue;
        }
        if !in_quote && ch.is_whitespace() {
            flush(&mut buf, &mut tokens, TokenKind::Fuzzy);
            continue;
        }
        buf.push(ch);
    }

    // If quotes were unbalanced, fall back to plain whitespace tokenization.
    if in_quote {
        return ParsedSearchQuery {
            mode: SearchMode::Tokens,
            tokens: trimmed
                .split_whitespace()
                .filter(|token| !token.is_empty())
                .map(|token| SearchToken { kind: TokenKind::Fuzzy, value: token.to_owned() })
                .collect(),
            regex: None,
            error: None,
        };
    }

    flush(&mut buf, &mut tokens, TokenKind::Fuzzy);
    ParsedSearchQuery { mode: SearchMode::Tokens, tokens, regex: None, error: None }
}

pub fn match_session(session: &SessionInfo, parsed: &ParsedSearchQuery) -> MatchResult {
    let text = get_session_search_text(session);

    if parsed.mode == SearchMode::Regex {
        let Some(regex) = &parsed.regex else {
            return MatchResult { matches: false, score: 0.0 };
        };
        return match regex.find(&text) {
            Some(found) => MatchResult { matches: true, score: found.start() as f64 * 0.1 },
            None => MatchResult { matches: false, score: 0.0 },
        };
    }

    if parsed.tokens.is_empty() {
        return MatchResult { matches: true, score: 0.0 };
    }

    let mut total_score = 0.0;
    let mut normalized_text: Option<String> = None;

    for token in &parsed.tokens {
        if token.kind == TokenKind::Phrase {
            let normalized = normalized_text.get_or_insert_with(|| normalize_whitespace_lower(&text));
            let phrase = normalize_whitespace_lower(&token.value);
            if phrase.is_empty() {
                continue;
            }
            let Some(index) = normalized.find(&phrase) else {
                return MatchResult { matches: false, score: 0.0 };
            };
            total_score += normalized[..index].chars().count() as f64 * 0.1;
            continue;
        }

        let matched = fuzzy_match(&token.value, &text);
        if !matched.matches {
            return MatchResult { matches: false, score: 0.0 };
        }
        total_score += matched.score;
    }

    MatchResult { matches: true, score: total_score }
}

pub fn filter_and_sort_sessions(
    sessions: &[SessionInfo],
    query: &str,
    sort_mode: SortMode,
    name_filter: NameFilter,
) -> Vec<SessionInfo> {
    let name_filtered: Vec<SessionInfo> = sessions
        .iter()
        .filter(|session| matches_name_filter(session, name_filter))
        .cloned()
        .collect();
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return name_filtered;
    }

    let parsed = parse_search_query(query);
    if parsed.error.is_some() {
        return Vec::new();
    }

    // Recent mode: filter only, keep incoming order.
    if sort_mode == SortMode::Recent {
        return name_filtered
            .into_iter()
            .filter(|session| match_session(session, &parsed).matches)
            .collect();
    }

    // Relevance mode: sort by score, tie-break by modified desc.
    let mut scored: Vec<(SessionInfo, f64)> = name_filtered
        .into_iter()
        .filter_map(|session| {
            let result = match_session(&session, &parsed);
            result.matches.then_some((session, result.score))
        })
        .collect();

    scored.sort_by(|(a, a_score), (b, b_score)| {
        a_score.total_cmp(b_score).then_with(|| b.modified_ms.cmp(&a.modified_ms))
    });

    scored.into_iter().map(|(session, _)| session).collect()
}
