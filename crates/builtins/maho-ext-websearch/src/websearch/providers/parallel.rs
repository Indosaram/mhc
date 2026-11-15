//! Port of senpi `websearch/websearch/providers/parallel.ts`.
use crate::websearch::providers::shared::{
    BuildContext, BuiltSearchRequest, SearchResultItem, append_domain_filters, collect,
    content_headers, result, unique_by_url,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const DEFAULT_URL: &str = "https://search.parallel.ai/mcp";
pub const SEARCH_URL: &str = "https://api.parallel.ai/v1beta/search";
const PARALLEL_BETA_HEADER: &str = "search-extract-2025-10-10";
const MAX_EXCERPT_CHARS: f64 = 10_000.;
const MAX_SNIPPET_CHARS: usize = 400;

fn source_policy(allowed: Option<&[String]>, blocked: Option<&[String]>) -> Option<Value> {
    match (
        allowed.filter(|d| !d.is_empty()),
        blocked.filter(|d| !d.is_empty()),
    ) {
        (Some(allowed), _) => Some(json!({"include_domains": allowed})),
        (None, Some(blocked)) => Some(json!({"exclude_domains": blocked})),
        (None, None) => None,
    }
}

fn mcp_request(ctx: &BuildContext<'_>) -> BuiltSearchRequest {
    BuiltSearchRequest {
        form: None,
        url: ctx.base_url.unwrap_or(DEFAULT_URL).into(),
        method: "POST",
        headers: BTreeMap::from([
            (
                "Accept".into(),
                "application/json, text/event-stream".into(),
            ),
            ("Content-Type".into(), "application/json".into()),
        ]),
        body: Some(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "web_search",
                "arguments": {
                    "objective": ctx.query,
                    "search_queries": [
                        append_domain_filters(ctx.query, ctx.allowed_domains, ctx.blocked_domains)
                    ],
                },
            },
        })),
    }
}

fn keyed_request(ctx: &BuildContext<'_>, api_key: Option<&str>) -> BuiltSearchRequest {
    let mut body = json!({
        "objective": ctx.query,
        "search_queries": [ctx.query],
        "mode": "fast",
        "excerpts": {"max_chars_per_result": MAX_EXCERPT_CHARS},
    });
    if let Some(policy) = source_policy(ctx.allowed_domains, ctx.blocked_domains) {
        body["source_policy"] = policy;
    }
    BuiltSearchRequest {
        form: None,
        url: ctx.base_url.unwrap_or(SEARCH_URL).into(),
        method: "POST",
        headers: content_headers(BTreeMap::from([
            ("x-api-key".into(), api_key.unwrap_or_default().into()),
            ("parallel-beta".into(), PARALLEL_BETA_HEADER.into()),
        ])),
        body: Some(body),
    }
}

pub fn build_request(ctx: &BuildContext<'_>) -> BuiltSearchRequest {
    match ctx.api_key.filter(|key| !key.is_empty()) {
        Some(api_key) => keyed_request(ctx, Some(api_key)),
        None => mcp_request(ctx),
    }
}

fn search_payload(data: &Value) -> Value {
    let Some(tool_result) = data["result"].as_object() else {
        return data.clone();
    };
    if let Some(structured) = tool_result
        .get("structuredContent")
        .filter(|v| v.is_object())
    {
        return structured.clone();
    }
    for part in tool_result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(text) = part["text"].as_str() else {
            continue;
        };
        if !text.trim_start().starts_with('{') {
            continue;
        }
        if let Ok(parsed) = serde_json::from_str::<Value>(text)
            && parsed.is_object()
        {
            return parsed;
        }
    }
    json!({})
}

fn snippet_of(excerpts: &[Value]) -> Option<String> {
    let text = excerpts
        .iter()
        .filter_map(Value::as_str)
        .map(|excerpt| excerpt.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|excerpt| !excerpt.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return None;
    }
    if text.chars().count() > MAX_SNIPPET_CHARS {
        let kept: String = text.chars().take(MAX_SNIPPET_CHARS - 1).collect();
        return Some(format!("{kept}…"));
    }
    Some(text)
}

pub fn response_error(data: &Value) -> Option<String> {
    if let Some(message) = data["error"]["message"]
        .as_str()
        .filter(|message| !message.is_empty())
    {
        return Some(format!("Parallel MCP error: {message}"));
    }
    if data["result"]["isError"].as_bool() != Some(true) {
        return None;
    }
    let message = data["result"]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|part| part["text"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ");
    let message = message.trim();
    Some(format!(
        "Parallel search error: {}",
        if message.is_empty() {
            "the search tool reported an error"
        } else {
            message
        }
    ))
}

pub fn normalize_response(data: &Value) -> Vec<SearchResultItem> {
    let items = search_payload(data)["results"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    unique_by_url(collect(items.iter().map(|item| {
        let url = item["url"].as_str()?;
        let title = item["title"].as_str().unwrap_or(url);
        let mut normalized = result(
            Some(title),
            Some(url),
            snippet_of(item["excerpts"].as_array().map_or(&[], Vec::as_slice)).as_deref(),
            None,
            None,
        )?;
        if let Some(published) = item["publish_date"].as_str() {
            normalized.published_at = Some(published.into());
        }
        Some(normalized)
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(api_key: Option<&'a str>, max: f64) -> BuildContext<'a> {
        BuildContext {
            query: "vector db",
            max_results: max,
            api_key,
            base_url: None,
            allowed_domains: None,
            blocked_domains: None,
        }
    }

    #[test]
    fn without_a_key_it_calls_the_mcp_tool_over_json_rpc() {
        let request = build_request(&ctx(None, 5.0));
        assert_eq!(request.url, DEFAULT_URL);
        assert_eq!(
            request.headers["Accept"],
            "application/json, text/event-stream"
        );
        assert_eq!(
            request.body.unwrap(),
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "web_search",
                    "arguments": {
                        "objective": "vector db",
                        "search_queries": ["vector db"],
                    },
                },
            })
        );
    }

    #[test]
    fn with_a_key_it_calls_the_beta_search_api() {
        let request = build_request(&ctx(Some("par-key"), 5.0));
        assert_eq!(request.url, SEARCH_URL);
        assert_eq!(request.headers["x-api-key"], "par-key");
        assert_eq!(request.headers["parallel-beta"], PARALLEL_BETA_HEADER);
        assert_eq!(
            request.body.unwrap(),
            json!({
                "objective": "vector db",
                "search_queries": ["vector db"],
                "mode": "fast",
                "excerpts": {"max_chars_per_result": 10_000.0},
            })
        );
    }

    #[test]
    fn include_domains_suppresses_exclusions() {
        let allowed = vec!["a.test".to_string()];
        let blocked = vec!["b.test".to_string()];
        let request = build_request(&BuildContext {
            query: "q",
            max_results: 3.0,
            api_key: Some("k"),
            base_url: None,
            allowed_domains: Some(&allowed),
            blocked_domains: Some(&blocked),
        });
        assert_eq!(
            request.body.unwrap()["source_policy"],
            json!({"include_domains": ["a.test"]})
        );
        let blocked_only = build_request(&BuildContext {
            query: "q",
            max_results: 3.0,
            api_key: Some("k"),
            base_url: None,
            allowed_domains: None,
            blocked_domains: Some(&blocked),
        });
        assert_eq!(
            blocked_only.body.unwrap()["source_policy"],
            json!({"exclude_domains": ["b.test"]})
        );
    }

    #[test]
    fn mcp_domains_join_the_query_terms() {
        let allowed = vec!["a.test".to_string()];
        let request = build_request(&BuildContext {
            query: "q",
            max_results: 3.0,
            api_key: None,
            base_url: None,
            allowed_domains: Some(&allowed),
            blocked_domains: None,
        });
        assert_eq!(
            request.body.unwrap()["params"]["arguments"]["search_queries"],
            json!(["q site:a.test"])
        );
    }

    #[test]
    fn reads_structured_content_first_then_content_json() {
        let structured =
            json!({"result": {"structuredContent": {"results": [{"url": "https://a.test"}]}}});
        assert_eq!(normalize_response(&structured).len(), 1);
        let text = json!({"result": {"content": [{"type": "text", "text": "{\"results\":[{\"url\":\"https://b.test\"}]}"}]}});
        assert_eq!(normalize_response(&text)[0].url, "https://b.test");
        let keyed = json!({"results": [{"url": "https://c.test"}]});
        assert_eq!(normalize_response(&keyed)[0].url, "https://c.test");
    }

    #[test]
    fn joins_excerpts_and_caps_the_snippet() {
        let long = "x".repeat(MAX_SNIPPET_CHARS + 20);
        let results = normalize_response(&json!({"results": [{
            "url": "https://a.test",
            "title": "A",
            "excerpts": [" one\n two ", long],
            "publish_date": "2026-02-02",
        }]}));
        let snippet = results[0].snippet.as_deref().expect("snippet");
        assert_eq!(snippet.chars().count(), MAX_SNIPPET_CHARS);
        assert!(snippet.starts_with("one two "));
        assert!(snippet.ends_with('…'));
        assert_eq!(results[0].published_at.as_deref(), Some("2026-02-02"));
    }

    #[test]
    fn reports_rpc_and_tool_errors() {
        assert_eq!(
            response_error(&json!({"error": {"message": "bad"}})).unwrap(),
            "Parallel MCP error: bad"
        );
        assert_eq!(
            response_error(&json!({"result": {"isError": true, "content": [{"text": "nope"}]}}))
                .unwrap(),
            "Parallel search error: nope"
        );
        assert_eq!(
            response_error(&json!({"result": {"isError": true}})).unwrap(),
            "Parallel search error: the search tool reported an error"
        );
        assert_eq!(response_error(&json!({"results": []})), None);
    }
}
