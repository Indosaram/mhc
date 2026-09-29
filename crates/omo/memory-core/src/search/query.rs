//! FTS-lite query parsing and scoring.

use serde::{Deserialize, Serialize};

use crate::support::time::parse_rfc3339;

/// Tool call representation within a searchable document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchToolCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// Harness-neutral message projection consumed by the search engine.
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

/// Tokenized query split into bare terms and double-quoted phrases.
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

/// Composes the searchable haystack: type + content + reasoning + summary + tool names/args + returns.
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

    let tool_return_text = stringify_search_value(document.tool_return.as_ref());
    if !tool_return_text.is_empty() {
        parts.push(tool_return_text);
    }

    let func_response_text = stringify_search_value(document.func_response.as_ref());
    if !func_response_text.is_empty() {
        parts.push(func_response_text);
    }

    parts.join("\n")
}

/// Normalizes text by lowercasing, collapsing runs of whitespace to a single space, and trimming.
pub fn normalize_text(value: &str) -> String {
    let lower = value.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    words.join(" ")
}

/// Splits a query into bare terms and double-quoted phrases.
pub fn parse_query(query: &str) -> ParsedQuery {
    let trimmed = query.trim();
    let mut terms: Vec<String> = Vec::new();
    let mut phrases: Vec<String> = Vec::new();
    let mut buffer = String::new();
    let mut in_quote = false;
    let mut saw_unclosed_quote = false;

    for character in trimmed.chars() {
        if character == '"' {
            let value = buffer.trim();
            if !value.is_empty() {
                if in_quote {
                    phrases.push(value.to_string());
                } else {
                    terms.push(value.to_string());
                }
            }
            buffer.clear();
            in_quote = !in_quote;
            continue;
        }

        if !in_quote && character.is_whitespace() {
            let value = buffer.trim();
            if !value.is_empty() {
                terms.push(value.to_string());
            }
            buffer.clear();
            continue;
        }

        buffer.push(character);
    }

    if in_quote {
        saw_unclosed_quote = true;
    }

    let value = buffer.trim();
    if !value.is_empty() {
        if in_quote {
            phrases.push(value.to_string());
        } else {
            terms.push(value.to_string());
        }
    }

    if saw_unclosed_quote {
        let fallback_terms: Vec<String> = trimmed
            .split_whitespace()
            .map(|term| term.trim().to_string())
            .filter(|term| !term.is_empty())
            .collect();
        return ParsedQuery {
            terms: fallback_terms,
            phrases: Vec::new(),
        };
    }

    ParsedQuery { terms, phrases }
}

/// Returns the letta score, or None when any term or phrase is absent.
pub fn match_score(text: &str, query: &ParsedQuery) -> Option<f64> {
    let haystack = normalize_text(text);
    if haystack.is_empty() {
        return None;
    }

    let mut score = 0.0;

    for raw_phrase in &query.phrases {
        let phrase = normalize_text(raw_phrase);
        if phrase.is_empty() {
            continue;
        }
        let byte_idx = haystack.find(&phrase)?;
        let index = haystack[..byte_idx].encode_utf16().count();
        score += (index as f64) * 0.1;
    }

    for raw_term in &query.terms {
        let term = normalize_text(raw_term);
        if term.is_empty() {
            continue;
        }
        let byte_idx = haystack.find(&term)?;
        let index = haystack[..byte_idx].encode_utf16().count();
        let term_length = term.encode_utf16().count();
        let bonus = 50usize.saturating_sub(term_length);
        score += (index as f64) + (bonus as f64);
    }

    Some(score)
}

/// Inclusive ISO range filter; missing or unparseable dates keep a message eligible.
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
    let Some(message_time) = parse_rfc3339(created_at) else {
        return true;
    };

    let start_time = match start_date {
        Some(date) => match parse_rfc3339(date) {
            Some(time) => Some(time),
            None => return true,
        },
        None => None,
    };

    let end_time = match end_date {
        Some(date) => match parse_rfc3339(date) {
            Some(time) => Some(time),
            None => return true,
        },
        None => None,
    };

    if let Some(start) = start_time
        && message_time < start
    {
        return false;
    }
    if let Some(end) = end_time
        && message_time > end
    {
        return false;
    }

    true
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
