//! FTS-lite search core: full scan over a transcript provider, letta-exact ranking.

use serde::{Deserialize, Serialize};

use super::query::{SearchDocument, date_in_range, match_score, parse_query, searchable_text};

pub const DEFAULT_LIMIT: i64 = 100;
const EPOCH: &str = "1970-01-01T00:00:00.000Z";

/// A conversation stream containing messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptConversation {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    pub messages: Vec<SearchDocument>,
}

/// Source of transcripts implemented per harness.
pub trait TranscriptProvider {
    fn list_conversations(&self) -> Vec<TranscriptConversation>;
}

/// Configuration options for searching transcripts.
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

/// A single search result entry with score and document reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub message_id: String,
    pub conversation_id: String,
    pub created_at: String,
    pub score: f64,
    pub document: SearchDocument,
}

struct ScoredDocument {
    document: SearchDocument,
    score: f64,
}

/// Scans transcripts from provider, returning matching messages ranked by score and date.
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

    let raw_limit = options.limit.unwrap_or(DEFAULT_LIMIT);
    let limit = raw_limit.max(0) as usize;
    if limit == 0 {
        return Vec::new();
    }

    let mut scored: Vec<ScoredDocument> = Vec::new();
    for conversation in provider.list_conversations() {
        if conversation.hidden == Some(true) && options.include_hidden != Some(true) {
            continue;
        }
        if let Some(target_cid) = &options.conversation_id
            && &conversation.id != target_cid
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
            scored.push(ScoredDocument { document, score });
        }
    }

    scored.sort_by(|left, right| match left.score.partial_cmp(&right.score) {
        Some(std::cmp::Ordering::Equal) | None => {
            let right_date = right.document.date.as_deref().unwrap_or("");
            let left_date = left.document.date.as_deref().unwrap_or("");
            right_date.cmp(left_date)
        }
        Some(ordering) => ordering,
    });

    scored.truncate(limit);

    scored
        .into_iter()
        .map(|entry| SearchResult {
            message_id: entry.document.id.clone(),
            conversation_id: entry.document.conversation_id.clone(),
            created_at: entry
                .document
                .date
                .clone()
                .unwrap_or_else(|| EPOCH.to_string()),
            score: entry.score,
            document: entry.document,
        })
        .collect()
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
