//! Port of senpi `websearch/websearch/providers/exa-mcp.ts` (pin 903b8336).
use crate::websearch::providers::shared::{
    BuildContext, BuiltSearchRequest, SearchResultItem, append_domain_filters, clamp, collect,
    json_count, result, unique_by_url,
};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const DEFAULT_URL: &str = "https://mcp.exa.ai/mcp";
const MAX_SNIPPET_CHARS: usize = 400;

fn field(section: &str, name: &str) -> Option<String> {
    let pattern = Regex::new(&format!(r"(?m)^{}:[ \t]*(.*)$", regex::escape(name))).ok()?;
    let value = pattern
        .captures(section)?
        .get(1)?
        .as_str()
        .trim()
        .to_owned();
    (!value.is_empty() && value != "N/A").then_some(value)
}

fn snippet_of(section: &str) -> Option<String> {
    let pattern = Regex::new(r"(?m)^(?:Highlights|Text|Summary):[ \t]*\n?((?s:.*))\z").ok()?;
    let text = pattern
        .captures(section)?
        .get(1)?
        .as_str()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return None;
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() > MAX_SNIPPET_CHARS {
        return Some(format!(
            "{}\u{2026}",
            String::from_utf16_lossy(&units[..MAX_SNIPPET_CHARS - 1])
        ));
    }
    Some(text)
}

fn content_text(data: &Value, separator: &str) -> String {
    data["result"]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|part| part["text"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(separator)
}

/// The anonymous MCP endpoint answers a JSON-RPC call over an event stream; read its first
/// JSON `data:` message (`responseFormat: "event-stream"`).
pub fn parse_body(body_text: &str) -> Value {
    if body_text.trim_start().starts_with('{') {
        return serde_json::from_str(body_text).unwrap_or_else(|_| json!({}));
    }
    body_text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .find_map(|data| {
            serde_json::from_str::<Value>(data.trim())
                .ok()
                .filter(Value::is_object)
        })
        .unwrap_or_else(|| json!({}))
}

/// Each hit is a `Title:` / `URL:` / `Published:` / `Highlights:` section of the tool's text.
pub fn normalize_response(data: &Value) -> Vec<SearchResultItem> {
    let text = content_text(data, "\n\n")
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let splitter = Regex::new(r"\n{2,}").expect("section splitter");
    let mut sections: Vec<String> = Vec::new();
    for piece in splitter.split(&text) {
        match sections.last_mut() {
            Some(last) if !piece.starts_with("Title:") => {
                last.push_str("\n\n");
                last.push_str(piece);
            }
            _ => sections.push(piece.to_owned()),
        }
    }
    unique_by_url(collect(
        sections
            .iter()
            .filter(|section| section.starts_with("Title:"))
            .map(|section| {
                let mut item = result(
                    field(section, "Title").as_deref(),
                    field(section, "URL").as_deref(),
                    snippet_of(section).as_deref(),
                    None,
                    None,
                )?;
                if let Some(published) =
                    field(section, "Published").or_else(|| field(section, "Published Date"))
                {
                    item.published_at = Some(published);
                }
                Some(item)
            }),
    ))
}

/// Reports an error the service returned inside a successful response (`responseError`).
pub fn response_error(data: &Value) -> Option<String> {
    if let Some(message) = data["error"]["message"]
        .as_str()
        .filter(|message| !message.is_empty())
    {
        return Some(format!("Exa MCP error: {message}"));
    }
    if data["result"]["isError"] != true {
        return None;
    }
    let message = content_text(data, " ");
    let message = message.trim();
    Some(format!(
        "Exa MCP error: {}",
        if message.is_empty() {
            "the search tool reported an error"
        } else {
            message
        }
    ))
}

pub fn build_request(ctx: &BuildContext<'_>) -> Result<BuiltSearchRequest, url::ParseError> {
    let mut url = url::Url::parse(ctx.base_url.unwrap_or(DEFAULT_URL))?;
    url.query_pairs_mut().append_pair("tools", "web_search_exa");
    Ok(BuiltSearchRequest {
        form: None,
        url: url.into(),
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
                "name": "web_search_exa",
                "arguments": {
                    "query": append_domain_filters(ctx.query, ctx.allowed_domains, ctx.blocked_domains),
                    "numResults": json_count(clamp(ctx.max_results, 1.0, 20.0)),
                },
            },
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_the_web_search_tool_over_json_rpc_without_any_key() {
        let built = build_request(&BuildContext {
            query: "typescript satisfies operator",
            max_results: 10.0,
            api_key: None,
            base_url: None,
            allowed_domains: None,
            blocked_domains: None,
        })
        .unwrap();
        assert_eq!(built.url, "https://mcp.exa.ai/mcp?tools=web_search_exa");
        assert_eq!(built.method, "POST");
        assert_eq!(
            built.headers["Accept"],
            "application/json, text/event-stream"
        );
        assert_eq!(
            built.body.unwrap(),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"web_search_exa","arguments":{"query":"typescript satisfies operator","numResults":10.0}}})
        );
    }

    #[test]
    fn normalizes_a_recorded_event_stream_answer() {
        let data = parse_body(include_str!(
            "../../../tests/fixtures/websearch/exa-mcp-results.sse"
        ));
        assert_eq!(response_error(&data), None);
        let results = normalize_response(&data);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].title, "Documentation - TypeScript 4.9");
        assert_eq!(
            results[0].url,
            "https://www.typescriptlang.org/docs/handbook/release-notes/typescript-4-9.html"
        );
        assert!(results[0].snippet.as_deref().unwrap().contains("satisfies"));
    }

    #[test]
    fn reports_rpc_and_tool_errors() {
        assert_eq!(
            response_error(&json!({"error":{"message":"bad"}})).as_deref(),
            Some("Exa MCP error: bad")
        );
        assert_eq!(
            response_error(&json!({"result":{"isError":true,"content":[]}})).as_deref(),
            Some("Exa MCP error: the search tool reported an error")
        );
        assert_eq!(
            response_error(
                &json!({"result":{"isError":true,"content":[{"text":" rate "},{"text":"limited"}]}})
            )
            .as_deref(),
            Some("Exa MCP error: rate  limited")
        );
    }
}
