//! Response normalization (spec §5): results, queries and usage from a Responses API payload.
//! Malformed shapes anywhere yield empty lists, never an error.

use regex::Regex;
use serde_json::{Value, json};
use std::sync::LazyLock;

const QUERY_TOOL_NAMES: [&str; 4] = [
    "x_keyword_search",
    "x_semantic_search",
    "x_user_search",
    "x_thread_fetch",
];
const QUERY_ITEM_TYPES: [&str; 2] = ["custom_tool_call", "x_search_call"];
const MAX_ID_DIGITS: usize = 30;

static POST_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"https?://(?:www\.)?x\.com/(?:i|[A-Za-z0-9_]{1,15})/status/([0-9]+)")
        .unwrap_or_else(|error| std::panic::panic_any(error.to_string()))
});

/// One found post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostResult {
    pub id: String,
    pub url: String,
    pub title: String,
    pub snippet: String,
}

impl PostResult {
    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "url": self.url, "title": self.title, "snippet": self.snippet})
    }
}

/// Server-side usage counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub x_search_calls: u64,
    pub cost_ticks: u64,
}

/// Everything extracted from one response.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Normalized {
    pub results: Vec<PostResult>,
    pub queries: Vec<String>,
    pub usage: Usage,
}

/// Every post URL in `text`, as (url, id), in order. A digit run longer than 30 is not a post id.
pub fn find_post_urls(text: &str) -> Vec<(String, String)> {
    POST_URL
        .captures_iter(text)
        .filter_map(|captures| {
            let whole = captures.get(0)?;
            let id = captures.get(1)?.as_str();
            (id.len() <= MAX_ID_DIGITS).then(|| (whole.as_str().to_owned(), id.to_owned()))
        })
        .collect()
}

/// The post (url, id) a single URL string names, when it is one.
fn post_url(url: &str) -> Option<(String, String)> {
    find_post_urls(url.trim()).into_iter().next()
}

fn message_items(response: &Value) -> impl Iterator<Item = &Value> {
    response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
}

fn message_contents(response: &Value) -> impl Iterator<Item = &Value> {
    message_items(response).flat_map(|item| {
        item.get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
    })
}

fn message_texts(response: &Value) -> Vec<&str> {
    message_contents(response)
        .filter_map(|content| content.get("text").and_then(Value::as_str))
        .collect()
}

fn is_plain_title(title: &str) -> bool {
    let title = title.trim();
    !title.is_empty()
        && !title.bytes().all(|byte| byte.is_ascii_digit())
        && !title.starts_with("http://")
        && !title.starts_with("https://")
}

/// Whether `line` mentions `needle` not directly followed by another digit (so `/status/1` does
/// not match inside `/status/12`).
fn mentions(line: &str, needle: &str) -> bool {
    line.match_indices(needle).any(|(start, _)| {
        !line[start + needle.len()..]
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_digit())
    })
}

fn snippet_for(lines: &[&str], url: &str, id: &str) -> String {
    let status = format!("/status/{id}");
    lines
        .iter()
        .find(|line| mentions(line, url) || mentions(line, &status))
        .map(|line| (*line).to_owned())
        .unwrap_or_default()
}

fn render_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// `k=v k=v ...` for each listed X search sub-call whose `input` is a JSON-object string.
pub fn extract_queries(response: &Value) -> Vec<String> {
    response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| {
            item.get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| QUERY_ITEM_TYPES.contains(&kind))
                && item
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| QUERY_TOOL_NAMES.contains(&name))
        })
        .filter_map(|item| {
            let input = item.get("input").and_then(Value::as_str)?;
            let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(input) else {
                return None;
            };
            Some(
                fields
                    .iter()
                    .map(|(key, value)| format!("{key}={}", render_value(value)))
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        })
        .collect()
}

/// The usage counters (0 when absent or malformed).
pub fn extract_usage(response: &Value) -> Usage {
    let usage = response.get("usage");
    Usage {
        x_search_calls: usage
            .and_then(|usage| usage.get("server_side_tool_usage_details"))
            .and_then(|details| details.get("x_search_calls"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cost_ticks: usage
            .and_then(|usage| usage.get("cost_in_usd_ticks"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

/// Results in discovery order (annotations, message text, top-level citations), deduplicated by
/// post id and truncated to `max_results`.
pub fn extract_results(response: &Value, max_results: usize) -> Vec<PostResult> {
    let texts = message_texts(response);
    let lines: Vec<&str> = texts
        .iter()
        .copied()
        .flat_map(str::lines)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let mut found: Vec<(String, String, Option<String>)> = Vec::new();
    let mut push = |url: String, id: String, title: Option<String>| {
        if !found.iter().any(|(_, seen, _)| *seen == id) {
            found.push((url, id, title));
        }
    };
    for content in message_contents(response) {
        for annotation in content
            .get("annotations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if annotation.get("type").and_then(Value::as_str) != Some("url_citation") {
                continue;
            }
            let Some((url, id)) = annotation
                .get("url")
                .and_then(Value::as_str)
                .and_then(post_url)
            else {
                continue;
            };
            let title = annotation
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_owned);
            push(url, id, title);
        }
    }
    for text in &texts {
        for (url, id) in find_post_urls(text) {
            push(url, id, None);
        }
    }
    for citation in response
        .get("citations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if let Some((url, id)) = post_url(citation) {
            push(url, id, None);
        }
    }
    found
        .into_iter()
        .take(max_results)
        .map(|(url, id, title)| PostResult {
            title: title
                .filter(|title| is_plain_title(title))
                .map(|title| title.trim().to_owned())
                .unwrap_or_else(|| url.clone()),
            snippet: snippet_for(&lines, &url, &id),
            id,
            url,
        })
        .collect()
}

/// Spec §5 over one parsed response.
pub fn normalize(response: &Value, max_results: usize) -> Normalized {
    Normalized {
        results: extract_results(response, max_results),
        queries: extract_queries(response),
        usage: extract_usage(response),
    }
}
