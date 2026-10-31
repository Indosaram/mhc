//! Port of senpi `websearch/websearch/providers/chatgpt-subscription.ts` (senpi 59d58cee59): web
//! search through the ChatGPT subscription login's Codex Responses endpoint, streamed.
use super::shared::{
    BuildContext, BuiltSearchRequest, SearchResultItem, append_domain_filters, collect, result,
};
use crate::websearch::provider_endpoints::{SearchProvider, provider_url};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const DEFAULT_MODEL: &str = "gpt-5.5";
const INSTRUCTIONS: &str = "You are a web search assistant. Always run the web_search tool for the user's query, then answer briefly and cite the pages you used.";

fn request_headers(api_key: &str) -> BTreeMap<String, String> {
    let identity = maho_ai::wire_identity::get_wire_identity();
    let mut headers = BTreeMap::from([
        ("Authorization".to_owned(), format!("Bearer {api_key}")),
        (
            "OpenAI-Beta".to_owned(),
            "responses=experimental".to_owned(),
        ),
        ("originator".to_owned(), identity.clone()),
        (
            "User-Agent".to_owned(),
            format!(
                "{identity} ({} {}; {})",
                std::env::consts::OS,
                os_release(),
                std::env::consts::ARCH
            ),
        ),
        ("Accept".to_owned(), "text/event-stream".to_owned()),
        ("Content-Type".to_owned(), "application/json".to_owned()),
    ]);
    if let Some(account) =
        maho_ai::utils::chatgpt_subscription_auth::extract_chatgpt_subscription_account_id(api_key)
    {
        headers.insert("chatgpt-account-id".to_owned(), account);
    }
    headers
}

/// senpi `withConfigHeaders(config, requestHeaders)`: the configured (session-login) headers, then
/// the request's own on top, names lowercased as `Headers` emits them; a configured
/// `chatgpt-account-id` is dropped when the token names no account.
pub fn with_config_headers(
    config: Option<&BTreeMap<String, String>>,
    request: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let has_account = request.contains_key("chatgpt-account-id");
    let mut merged: BTreeMap<String, String> = config
        .into_iter()
        .flatten()
        .map(|(name, value)| (name.to_lowercase(), value.clone()))
        .collect();
    merged.extend(
        request
            .into_iter()
            .map(|(name, value)| (name.to_lowercase(), value)),
    );
    if !has_account {
        merged.remove("chatgpt-account-id");
    }
    merged
}

/// node `os.release()`: the kernel release where the platform exposes it, else empty.
fn os_release() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|release| release.trim().to_owned())
        .unwrap_or_default()
}

pub fn build_request(
    ctx: &BuildContext<'_>,
    model: Option<&str>,
    codex_mode: Option<&str>,
    search_context_size: Option<&str>,
    user_location: Option<&Value>,
) -> BuiltSearchRequest {
    let mut tool = json!({"type": "web_search", "external_web_access": codex_mode.unwrap_or("live") == "live"});
    if let Some(size) = search_context_size.filter(|size| !size.is_empty()) {
        tool["search_context_size"] = json!(size);
    }
    if let Some(domains) = ctx.allowed_domains {
        tool["filters"] = json!({"allowed_domains": domains});
    }
    if let Some(location) = user_location {
        let mut merged = serde_json::Map::from_iter([("type".to_owned(), json!("approximate"))]);
        merged.extend(location.as_object().cloned().unwrap_or_default());
        tool["user_location"] = Value::Object(merged);
    }
    let query = match ctx.blocked_domains {
        Some(blocked) => append_domain_filters(ctx.query, None, Some(blocked)),
        None => ctx.query.to_owned(),
    };
    BuiltSearchRequest {
        url: provider_url(SearchProvider::ChatgptSubscription, ctx.base_url).to_owned(),
        method: "POST",
        headers: request_headers(ctx.api_key.unwrap_or_default()),
        body: Some(json!({
            "model": model.unwrap_or(DEFAULT_MODEL),
            "instructions": INSTRUCTIONS,
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": query}]}],
            "tools": [tool],
            "tool_choice": {"type": "web_search"},
            "include": ["web_search_call.action.sources"],
            "store": false,
            "stream": true,
        })),
        form: None,
    }
}

fn clean_source_url(raw: &str) -> String {
    let Ok(mut url) = url::Url::parse(raw) else {
        return raw.to_owned();
    };
    if url
        .query_pairs()
        .any(|(key, value)| key == "utm_source" && value == "openai")
    {
        let kept: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "utm_source")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        if kept.is_empty() {
            url.set_query(None);
        } else {
            url.query_pairs_mut().clear().extend_pairs(kept);
        }
    }
    url.into()
}

/// JS `text.slice(max(0, start - 100), min(len, end + 100)).trim()` over UTF-16 code units.
fn citation_snippet(text: &str, start: Option<f64>, end: Option<f64>) -> Option<String> {
    let (start, end) = (start?, end?);
    let units: Vec<u16> = text.encode_utf16().collect();
    let from = ((start - 100.0).max(0.0) as usize).min(units.len());
    let to = ((end + 100.0).max(0.0) as usize).min(units.len());
    let snippet = String::from_utf16_lossy(&units[from..to.max(from)]);
    let snippet = snippet.trim();
    (!snippet.is_empty()).then(|| snippet.to_owned())
}

fn array(value: Option<&Value>) -> &[Value] {
    value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// Only a reply that ran web_search counts; its citations and search sources are the results,
/// never URLs typed in the answer.
pub fn normalize_response(data: &Value) -> Vec<SearchResultItem> {
    let output = array(data.get("output"));
    let calls: Vec<&Value> = output
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("web_search_call"))
        .collect();
    if calls.is_empty() {
        return Vec::new();
    }
    let citations = output
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .flat_map(|item| array(item.get("content")))
        .flat_map(|part| {
            let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
            array(part.get("annotations"))
                .iter()
                .map(move |annotation| {
                    let url = annotation
                        .get("url")
                        .and_then(Value::as_str)
                        .filter(|url| !url.is_empty());
                    if annotation.get("type").and_then(Value::as_str) != Some("url_citation") {
                        return None;
                    }
                    let url = url?;
                    let snippet = citation_snippet(
                        text,
                        annotation.get("start_index").and_then(Value::as_f64),
                        annotation.get("end_index").and_then(Value::as_f64),
                    );
                    let title = annotation
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or(url);
                    result(
                        Some(title),
                        Some(clean_source_url(url).as_str()),
                        snippet.as_deref(),
                        None,
                        None,
                    )
                })
        });
    let sources = calls
        .iter()
        .flat_map(|call| array(call.get("action").and_then(|action| action.get("sources"))))
        .map(|source| {
            let url = source
                .get("url")
                .and_then(Value::as_str)
                .filter(|url| !url.is_empty())?;
            let title = source.get("title").and_then(Value::as_str).unwrap_or(url);
            result(
                Some(title),
                Some(clean_source_url(url).as_str()),
                None,
                None,
                None,
            )
        });
    let mut seen = std::collections::HashSet::new();
    let unique = citations
        .chain(sources)
        .flatten()
        .filter(|item| seen.insert(item.url.clone()))
        .map(Some);
    collect(unique)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(allowed: Option<&'a [String]>, blocked: Option<&'a [String]>) -> BuildContext<'a> {
        BuildContext {
            query: "rust",
            max_results: 5.0,
            api_key: Some("token"),
            base_url: None,
            allowed_domains: allowed,
            blocked_domains: blocked,
        }
    }

    #[test]
    fn request_streams_a_forced_web_search_to_the_codex_endpoint() {
        let allowed = vec!["docs.rs".to_owned()];
        let built = build_request(
            &ctx(Some(&allowed), None),
            None,
            Some("cached"),
            Some("low"),
            Some(&json!({"country": "US"})),
        );
        assert_eq!(built.url, "https://chatgpt.com/backend-api/codex/responses");
        assert_eq!(built.headers["Authorization"], "Bearer token");
        assert_eq!(built.headers["Accept"], "text/event-stream");
        assert!(!built.headers.contains_key("chatgpt-account-id"));
        let body = built.body.expect("body");
        assert_eq!(body["model"], DEFAULT_MODEL);
        assert_eq!(body["stream"], true);
        assert_eq!(body["tool_choice"], json!({"type": "web_search"}));
        assert_eq!(
            body["tools"][0],
            json!({"type": "web_search", "external_web_access": false, "search_context_size": "low",
                "filters": {"allowed_domains": ["docs.rs"]}, "user_location": {"type": "approximate", "country": "US"}})
        );
        assert_eq!(body["input"][0]["content"][0]["text"], "rust");
    }

    #[test]
    fn request_headers_win_over_configured_ones_and_stale_account_ids_drop() {
        let config = BTreeMap::from([
            ("Authorization".to_owned(), "Bearer stale".to_owned()),
            ("chatgpt-account-id".to_owned(), "acct-stale".to_owned()),
            ("X-Extra".to_owned(), "1".to_owned()),
        ]);
        let built = build_request(&ctx(None, None), None, None, None, None);
        let merged = with_config_headers(Some(&config), built.headers);
        assert_eq!(merged["authorization"], "Bearer token");
        assert_eq!(merged["x-extra"], "1");
        assert!(!merged.contains_key("chatgpt-account-id"));
    }

    #[test]
    fn blocked_domains_go_into_the_query() {
        let blocked = vec!["spam.example".to_owned()];
        let built = build_request(
            &ctx(None, Some(&blocked)),
            Some("gpt-5.6"),
            None,
            None,
            None,
        );
        let body = built.body.expect("body");
        assert_eq!(body["model"], "gpt-5.6");
        assert_eq!(body["tools"][0]["external_web_access"], true);
        assert_ne!(body["input"][0]["content"][0]["text"], "rust");
    }

    #[test]
    fn results_need_a_search_call_and_dedupe_citations_before_sources() {
        let answer_only = json!({"output": [{"type": "message", "content": [{"type": "output_text", "text": "see https://x.example", "annotations": [
            {"type": "url_citation", "url": "https://x.example", "title": "X", "start_index": 4, "end_index": 21}]}]}]});
        assert!(normalize_response(&answer_only).is_empty());
        let searched = json!({"output": [
            {"type": "web_search_call", "action": {"sources": [{"url": "https://a.example/?utm_source=openai"}, {"url": "https://b.example/", "title": "B"}]}},
            {"type": "message", "content": [{"type": "output_text", "text": "Answer cites A.", "annotations": [
                {"type": "url_citation", "url": "https://a.example/?utm_source=openai", "title": "A", "start_index": 0, "end_index": 6},
                {"type": "file_citation", "url": "https://ignored.example/"}]}]}]});
        let results = normalize_response(&searched);
        assert_eq!(
            results
                .iter()
                .map(|item| (item.title.as_str(), item.url.as_str()))
                .collect::<Vec<_>>(),
            [("A", "https://a.example/"), ("B", "https://b.example/")]
        );
        assert_eq!(results[0].snippet.as_deref(), Some("Answer cites A."));
    }
}
