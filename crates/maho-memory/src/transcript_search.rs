//! Query parsing and ranking for searching past conversations.
//!
//! Spec: `docs/cleanroom/specs/memory-transcript-search.md`. The ranking is deliberately odd and deliberately
//! kept: a message scores *higher* the **later** a term appears, so a hit near the top of a long transcript
//! ranks below the same hit near the bottom. This is the reference behaviour, not a bug to be fixed here.
//!
//! `search_transcripts` is the same ranking applied over a conversation provider: filter by hidden and
//! conversation id, score every message, keep only in-range dates, then sort by score and break ties by the
//! **later** date, so an undated message sorts last.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchToolCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchDocument {
    pub id: String,
    pub conversation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<SearchToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_return: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub func_response: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedQuery {
    pub terms: Vec<String>,
    pub phrases: Vec<String>,
}

fn text_from_content(content: Option<&serde_json::Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    match content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => {
            let mut lines = Vec::new();
            for part in parts {
                if let serde_json::Value::Object(map) = part
                    && let Some(serde_json::Value::String(text)) = map.get("text")
                    && !text.is_empty()
                {
                    lines.push(text.as_str());
                }
            }
            lines.join("\n")
        }
        _ => String::new(),
    }
}

fn stringify_search_value(value: Option<&serde_json::Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Everything a message can be matched on, joined by newlines.
///
/// Tool arguments and returns are included: "which command produced this" is a common thing to search for, and
/// the answer lives in the call rather than the prose.
pub fn searchable_text(document: &SearchDocument) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(message_type) = &document.message_type
        && !message_type.is_empty()
    {
        parts.push(message_type.clone());
    }
    let content_text = text_from_content(document.content.as_ref());
    if !content_text.is_empty() {
        parts.push(content_text);
    }
    if let Some(reasoning) = &document.reasoning
        && !reasoning.is_empty()
    {
        parts.push(reasoning.clone());
    }
    if let Some(summary) = &document.summary
        && !summary.is_empty()
    {
        parts.push(summary.clone());
    }
    if let Some(tool_calls) = &document.tool_calls {
        for call in tool_calls {
            if let Some(name) = &call.name
                && !name.is_empty()
            {
                parts.push(name.clone());
            }
            if let Some(arguments) = &call.arguments
                && !arguments.is_empty()
            {
                parts.push(arguments.clone());
            }
        }
    }
    for value in [
        document.tool_return.as_ref(),
        document.func_response.as_ref(),
    ] {
        let text = stringify_search_value(value);
        if !text.is_empty() {
            parts.push(text);
        }
    }
    parts.join("\n")
}

/// Lowercased with whitespace runs collapsed. Idempotent, which is what lets a caller normalize once.
pub fn normalize_text(value: &str) -> String {
    value
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Split into bare terms and `"quoted phrases"`.
///
/// An unclosed quote abandons phrase parsing entirely and falls back to plain whitespace splitting over the
/// whole query. Half a phrase would otherwise swallow the rest of the query, and a user who typed one `"` by
/// accident should still get results.
pub fn parse_query(query: &str) -> ParsedQuery {
    let trimmed = query.trim();
    let mut terms: Vec<String> = Vec::new();
    let mut phrases: Vec<String> = Vec::new();
    let mut buffer = String::new();
    let mut in_quote = false;

    for character in trimmed.chars() {
        if character == '"' {
            let value = buffer.trim();
            if !value.is_empty() {
                if in_quote {
                    phrases.push(value.to_owned());
                } else {
                    terms.push(value.to_owned());
                }
            }
            buffer.clear();
            in_quote = !in_quote;
            continue;
        }
        if !in_quote && character.is_whitespace() {
            let value = buffer.trim();
            if !value.is_empty() {
                terms.push(value.to_owned());
            }
            buffer.clear();
            continue;
        }
        buffer.push(character);
    }

    let value = buffer.trim();
    if !value.is_empty() {
        if in_quote {
            phrases.push(value.to_owned());
        } else {
            terms.push(value.to_owned());
        }
    }

    if in_quote {
        let fallback_terms: Vec<String> = trimmed
            .split_whitespace()
            .map(|term| term.trim().to_owned())
            .filter(|term| !term.is_empty())
            .collect();
        return ParsedQuery {
            terms: fallback_terms,
            phrases: Vec::new(),
        };
    }

    ParsedQuery { terms, phrases }
}

/// The score, or `None` when any term or phrase is absent.
///
/// Every term and every phrase must appear: this is an AND, not a relevance blend. A message missing one term
/// is not a weaker match, it is not a match.
pub fn match_score(text: &str, query: &ParsedQuery) -> Option<f64> {
    match_score_normalized(&normalize_text(text), query)
}

/// [`match_score`] over a haystack already normalized.
pub fn match_score_normalized(haystack: &str, query: &ParsedQuery) -> Option<f64> {
    if haystack.is_empty() {
        return None;
    }
    let mut score = 0.0;
    for raw_phrase in &query.phrases {
        let phrase = normalize_text(raw_phrase);
        if phrase.is_empty() {
            continue;
        }
        let byte_index = haystack.find(&phrase)?;
        // UTF-16 offset, because the reference ranking was computed over JS strings.
        let index = haystack[..byte_index].encode_utf16().count();
        score += (index as f64) * 0.1;
    }
    for raw_term in &query.terms {
        let term = normalize_text(raw_term);
        if term.is_empty() {
            continue;
        }
        let byte_index = haystack.find(&term)?;
        let index = haystack[..byte_index].encode_utf16().count();
        // Short terms score higher, so a distinctive short word outranks a common long one.
        let term_length = term.encode_utf16().count();
        let bonus = 50usize.saturating_sub(term_length);
        score += (index as f64) + (bonus as f64);
    }
    Some(score)
}

/// Inclusive ISO-8601 range filter.
///
/// A message with no date, or an unparseable one, stays **eligible**. Search is for finding things; hiding a
/// message because its timestamp is malformed is the worse failure. The same applies to a malformed bound: it
/// is ignored rather than excluding everything.
pub fn date_in_range(
    created_at: Option<&str>,
    start_date: Option<&str>,
    end_date: Option<&str>,
) -> bool {
    if start_date.is_none() && end_date.is_none() {
        return true;
    }
    let Some(created_at) = created_at else {
        return true;
    };
    let Some(message_time) = chrono::DateTime::parse_from_rfc3339(created_at).ok() else {
        return true;
    };
    let bound = |value: Option<&str>| match value {
        Some(date) => chrono::DateTime::parse_from_rfc3339(date).ok(),
        None => None,
    };
    let (start, end) = (bound(start_date), bound(end_date));
    if start_date.is_some() && start.is_none() {
        return true;
    }
    if end_date.is_some() && end.is_none() {
        return true;
    }
    if let Some(start) = start
        && message_time < start
    {
        return false;
    }
    if let Some(end) = end
        && message_time > end
    {
        return false;
    }
    true
}

/// A conversation stream containing messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptConversation {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    pub messages: Vec<SearchDocument>,
}

/// Source of transcripts, implemented per harness.
pub trait TranscriptProvider {
    fn list_conversations(&self) -> Vec<TranscriptConversation>;
}

/// Filters for a transcript search.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_hidden: Option<bool>,
}

/// One ranked message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub message_id: String,
    pub conversation_id: String,
    pub created_at: String,
    pub score: f64,
    pub document: SearchDocument,
}

/// Default number of results when the caller names no limit.
pub const DEFAULT_SEARCH_LIMIT: i64 = 100;

const EPOCH: &str = "1970-01-01T00:00:00.000Z";

/// Ranks every message in `provider` against `query`.
///
/// An empty or unparseable query, and a limit of zero, return nothing rather than everything — a search that
/// matches no term must not look like a successful full dump.
pub fn search_transcripts(
    provider: &dyn TranscriptProvider,
    query: &str,
    options: &SearchOptions,
) -> Vec<SearchResult> {
    let query_text = query.trim();
    if query_text.is_empty() {
        return Vec::new();
    }
    let parsed = parse_query(query_text);
    if parsed.terms.is_empty() && parsed.phrases.is_empty() {
        return Vec::new();
    }
    let limit = options.limit.unwrap_or(DEFAULT_SEARCH_LIMIT).max(0) as usize;
    if limit == 0 {
        return Vec::new();
    }

    let mut scored: Vec<(SearchDocument, f64)> = Vec::new();
    for conversation in provider.list_conversations() {
        if conversation.hidden == Some(true) && options.include_hidden != Some(true) {
            continue;
        }
        if let Some(target) = &options.conversation_id
            && &conversation.id != target
        {
            continue;
        }
        for document in conversation.messages {
            let Some(score) = match_score(&searchable_text(&document), &parsed) else {
                continue;
            };
            if !date_in_range(
                document.date.as_deref(),
                options.start_date.as_deref(),
                options.end_date.as_deref(),
            ) {
                continue;
            }
            scored.push((document, score));
        }
    }

    scored.sort_by(|(left, left_score), (right, right_score)| {
        match left_score.partial_cmp(right_score) {
            Some(std::cmp::Ordering::Equal) | None => {
                let right_date = right.date.as_deref().unwrap_or("");
                let left_date = left.date.as_deref().unwrap_or("");
                right_date.cmp(left_date)
            }
            Some(ordering) => ordering,
        }
    });
    scored.truncate(limit);

    scored
        .into_iter()
        .map(|(document, score)| SearchResult {
            message_id: document.id.clone(),
            conversation_id: document.conversation_id.clone(),
            created_at: document.date.clone().unwrap_or_else(|| EPOCH.to_string()),
            score,
            document,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(content: &str) -> SearchDocument {
        SearchDocument {
            id: "m1".to_owned(),
            conversation_id: "c1".to_owned(),
            date: None,
            message_type: None,
            content: Some(serde_json::Value::String(content.to_owned())),
            reasoning: None,
            summary: None,
            tool_calls: None,
            tool_return: None,
            func_response: None,
        }
    }

    fn query(terms: &[&str], phrases: &[&str]) -> ParsedQuery {
        ParsedQuery {
            terms: terms.iter().map(|term| (*term).to_owned()).collect(),
            phrases: phrases.iter().map(|phrase| (*phrase).to_owned()).collect(),
        }
    }

    #[test]
    fn bare_words_become_terms() {
        assert_eq!(
            parse_query("memory store"),
            query(&["memory", "store"], &[])
        );
    }

    #[test]
    fn a_quoted_run_becomes_one_phrase() {
        assert_eq!(
            parse_query("\"memory store\" fix"),
            query(&["fix"], &["memory store"])
        );
    }

    #[test]
    fn extra_whitespace_is_ignored() {
        assert_eq!(parse_query("  a   b  "), query(&["a", "b"], &[]));
    }

    #[test]
    fn an_empty_query_has_no_terms() {
        assert_eq!(parse_query("   "), query(&[], &[]));
        assert_eq!(parse_query(""), query(&[], &[]));
    }

    #[test]
    fn an_unclosed_quote_falls_back_to_plain_terms() {
        assert_eq!(
            parse_query("\"memory store"),
            query(&["\"memory", "store"], &[])
        );
    }

    #[test]
    fn an_empty_quoted_run_is_dropped() {
        assert_eq!(parse_query("\"\""), query(&[], &[]));
    }

    #[test]
    fn normalization_lowercases_and_collapses_whitespace() {
        assert_eq!(normalize_text("  Hello   WORLD \n"), "hello world");
    }

    #[test]
    fn normalization_is_idempotent() {
        let once = normalize_text(" A  B ");
        assert_eq!(normalize_text(&once), once);
    }

    #[test]
    fn every_term_must_be_present() {
        assert!(match_score("the memory store", &query(&["memory", "store"], &[])).is_some());
        assert!(match_score("the memory bank", &query(&["memory", "store"], &[])).is_none());
    }

    #[test]
    fn every_phrase_must_be_present() {
        assert!(match_score("the memory store", &query(&[], &["memory store"])).is_some());
        assert!(match_score("the memory bank", &query(&[], &["memory store"])).is_none());
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(match_score("MEMORY", &query(&["memory"], &[])).is_some());
    }

    #[test]
    fn an_empty_haystack_never_matches() {
        assert!(match_score("", &query(&["a"], &[])).is_none());
    }

    #[test]
    fn a_later_match_scores_higher_than_an_earlier_one() {
        let early = match_score("memory aaaa", &query(&["memory"], &[])).expect("match");
        let late = match_score("aaaa memory", &query(&["memory"], &[])).expect("match");
        assert!(late > early, "early={early} late={late}");
    }

    #[test]
    fn a_shorter_term_earns_a_bigger_bonus() {
        let short = match_score("ab", &query(&["ab"], &[])).expect("match");
        let long = match_score("conservation", &query(&["conservation"], &[])).expect("match");
        assert!(short > long, "short={short} long={long}");
    }

    #[test]
    fn a_long_term_does_not_go_negative() {
        let long = "a".repeat(80);
        let score = match_score(&long, &query(&[long.as_str()], &[])).expect("match");
        assert!(score >= 0.0, "{score}");
    }

    #[test]
    fn a_phrase_contributes_a_tenth_of_its_offset() {
        let score = match_score("abcdefghij x", &query(&[], &["x"])).expect("match");
        assert!((score - 1.1).abs() < 1e-9, "{score}");
    }

    #[test]
    fn terms_and_phrases_add_together() {
        let score = match_score("ab cd", &query(&["ab"], &["cd"])).expect("match");
        assert!(score > 0.0);
    }

    #[test]
    fn an_empty_term_is_skipped_rather_than_matching_everything() {
        assert!(match_score("text", &query(&[""], &[])).is_some());
    }

    #[test]
    fn offsets_are_counted_in_utf16_units() {
        let astral = "\u{1f600}";
        let score = match_score(&format!("{astral} x"), &query(&["x"], &[])).expect("match");
        // One astral character plus a space is 3 UTF-16 units.
        assert!((score - (3.0 + 49.0)).abs() < 1e-9, "{score}");
    }

    #[test]
    fn the_haystack_covers_type_content_reasoning_summary_and_tools() {
        let mut document = document("body");
        document.message_type = Some("assistant".to_owned());
        document.reasoning = Some("thinking".to_owned());
        document.summary = Some("summary".to_owned());
        document.tool_calls = Some(vec![SearchToolCall {
            name: Some("bash".to_owned()),
            arguments: Some("ls -la".to_owned()),
        }]);
        document.tool_return = Some(serde_json::Value::String("returned".to_owned()));
        document.func_response = Some(serde_json::Value::String("responded".to_owned()));
        let text = searchable_text(&document);
        for expected in [
            "assistant",
            "body",
            "thinking",
            "summary",
            "bash",
            "ls -la",
            "returned",
            "responded",
        ] {
            assert!(text.contains(expected), "missing {expected} in {text:?}");
        }
    }

    #[test]
    fn structured_content_reads_its_text_parts() {
        let mut document = document("");
        document.content = Some(serde_json::json!([
            {"type": "text", "text": "first"},
            {"type": "text", "text": ""},
            {"type": "image"}
        ]));
        assert_eq!(searchable_text(&document), "first");
    }

    #[test]
    fn a_null_return_contributes_nothing() {
        let mut document = document("x");
        document.tool_return = Some(serde_json::Value::Null);
        assert_eq!(searchable_text(&document), "x");
    }

    #[test]
    fn a_structured_return_is_serialized() {
        let mut document = document("x");
        document.tool_return = Some(serde_json::json!({"code": 0}));
        assert!(searchable_text(&document).contains("\"code\":0"));
    }

    #[test]
    fn an_empty_range_includes_everything() {
        assert!(date_in_range(Some("2026-01-01T00:00:00Z"), None, None));
    }

    #[test]
    fn a_message_inside_the_range_is_included() {
        assert!(date_in_range(
            Some("2026-05-01T00:00:00Z"),
            Some("2026-01-01T00:00:00Z"),
            Some("2026-12-31T00:00:00Z")
        ));
    }

    #[test]
    fn a_message_before_the_start_is_excluded() {
        assert!(!date_in_range(
            Some("2025-12-31T00:00:00Z"),
            Some("2026-01-01T00:00:00Z"),
            None
        ));
    }

    #[test]
    fn a_message_after_the_end_is_excluded() {
        assert!(!date_in_range(
            Some("2027-01-01T00:00:00Z"),
            None,
            Some("2026-12-31T00:00:00Z")
        ));
    }

    fn doc(id: &str, conversation: &str, date: Option<&str>, content: &str) -> SearchDocument {
        SearchDocument {
            id: id.into(),
            conversation_id: conversation.into(),
            date: date.map(str::to_owned),
            message_type: None,
            content: Some(serde_json::Value::String(content.into())),
            reasoning: None,
            summary: None,
            tool_calls: None,
            tool_return: None,
            func_response: None,
        }
    }

    struct Provider(Vec<TranscriptConversation>);

    impl TranscriptProvider for Provider {
        fn list_conversations(&self) -> Vec<TranscriptConversation> {
            self.0.clone()
        }
    }

    fn conversation(
        id: &str,
        hidden: Option<bool>,
        messages: Vec<SearchDocument>,
    ) -> TranscriptConversation {
        TranscriptConversation {
            id: id.into(),
            hidden,
            messages,
        }
    }

    #[test]
    fn a_query_that_matches_nothing_returns_nothing() {
        let provider = Provider(vec![conversation(
            "c1",
            None,
            vec![doc("m1", "c1", None, "unrelated words")],
        )]);
        assert!(search_transcripts(&provider, "zebra", &SearchOptions::default()).is_empty());
    }

    #[test]
    fn an_empty_query_returns_nothing() {
        let provider = Provider(vec![conversation(
            "c1",
            None,
            vec![doc("m1", "c1", None, "anything")],
        )]);
        assert!(search_transcripts(&provider, "   ", &SearchOptions::default()).is_empty());
    }

    #[test]
    fn a_zero_limit_returns_nothing() {
        let provider = Provider(vec![conversation(
            "c1",
            None,
            vec![doc("m1", "c1", None, "zebra")],
        )]);
        let options = SearchOptions {
            limit: Some(0),
            ..SearchOptions::default()
        };
        assert!(search_transcripts(&provider, "zebra", &options).is_empty());
    }

    #[test]
    fn a_hidden_conversation_is_skipped_unless_asked_for() {
        let provider = Provider(vec![conversation(
            "c1",
            Some(true),
            vec![doc("m1", "c1", None, "zebra")],
        )]);
        assert!(search_transcripts(&provider, "zebra", &SearchOptions::default()).is_empty());
        let options = SearchOptions {
            include_hidden: Some(true),
            ..SearchOptions::default()
        };
        assert_eq!(search_transcripts(&provider, "zebra", &options).len(), 1);
    }

    #[test]
    fn a_conversation_filter_keeps_only_that_conversation() {
        let provider = Provider(vec![
            conversation("c1", None, vec![doc("m1", "c1", None, "zebra")]),
            conversation("c2", None, vec![doc("m2", "c2", None, "zebra")]),
        ]);
        let options = SearchOptions {
            conversation_id: Some("c2".into()),
            ..SearchOptions::default()
        };
        let results = search_transcripts(&provider, "zebra", &options);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].conversation_id, "c2");
    }

    #[test]
    fn a_date_window_excludes_a_message_outside_it() {
        let provider = Provider(vec![conversation(
            "c1",
            None,
            vec![
                doc("old", "c1", Some("2025-01-01T00:00:00Z"), "zebra"),
                doc("new", "c1", Some("2026-06-01T00:00:00Z"), "zebra"),
            ],
        )]);
        let options = SearchOptions {
            start_date: Some("2026-01-01T00:00:00Z".into()),
            ..SearchOptions::default()
        };
        let results = search_transcripts(&provider, "zebra", &options);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].message_id, "new");
    }

    #[test]
    fn an_undated_result_reports_the_epoch() {
        let provider = Provider(vec![conversation(
            "c1",
            None,
            vec![doc("m1", "c1", None, "zebra")],
        )]);
        let results = search_transcripts(&provider, "zebra", &SearchOptions::default());
        assert_eq!(results[0].created_at, "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn an_equal_score_sorts_the_later_date_first() {
        let provider = Provider(vec![conversation(
            "c1",
            None,
            vec![
                doc("early", "c1", Some("2026-01-01T00:00:00Z"), "zebra"),
                doc("late", "c1", Some("2026-09-01T00:00:00Z"), "zebra"),
            ],
        )]);
        let results = search_transcripts(&provider, "zebra", &SearchOptions::default());
        assert_eq!(results[0].message_id, "late");
    }

    #[test]
    fn the_limit_truncates_the_ranked_list() {
        let provider = Provider(vec![conversation(
            "c1",
            None,
            (0..5)
                .map(|index| {
                    doc(
                        &format!("m{index}"),
                        "c1",
                        Some("2026-01-01T00:00:00Z"),
                        "zebra",
                    )
                })
                .collect(),
        )]);
        let options = SearchOptions {
            limit: Some(2),
            ..SearchOptions::default()
        };
        assert_eq!(search_transcripts(&provider, "zebra", &options).len(), 2);
    }

    #[test]
    fn the_range_bounds_are_inclusive() {
        let bound = "2026-01-01T00:00:00Z";
        assert!(date_in_range(Some(bound), Some(bound), Some(bound)));
    }

    #[test]
    fn a_missing_or_unparseable_date_stays_eligible() {
        assert!(date_in_range(None, Some("2026-01-01T00:00:00Z"), None));
        assert!(date_in_range(
            Some("not a date"),
            Some("2026-01-01T00:00:00Z"),
            None
        ));
    }

    #[test]
    fn an_unparseable_bound_is_ignored() {
        assert!(date_in_range(
            Some("2026-05-01T00:00:00Z"),
            Some("nonsense"),
            None
        ));
        assert!(date_in_range(
            Some("2026-05-01T00:00:00Z"),
            None,
            Some("nonsense")
        ));
    }
}
