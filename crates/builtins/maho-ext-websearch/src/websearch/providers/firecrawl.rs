//! Port of senpi `websearch/websearch/providers/firecrawl.ts`.
use crate::websearch::providers::shared::{
    BuildContext, BuiltSearchRequest, SearchResultItem, append_domain_filters, clamp, collect,
    content_headers, result,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const DEFAULT_URL: &str = "https://api.firecrawl.dev/v2/search";

fn web_results(data: &Value) -> Vec<&Value> {
    let nested = data["data"]["web"].as_array();
    let rows = match nested.filter(|rows| !rows.is_empty()) {
        Some(rows) => Some(rows),
        None => data["data"]
            .as_array()
            .filter(|rows| !rows.is_empty())
            .or_else(|| data["results"].as_array()),
    };
    rows.into_iter()
        .flatten()
        .filter(|row| row.is_object())
        .collect()
}

pub fn response_error(data: &Value) -> Option<String> {
    if data["success"].as_bool() != Some(false) {
        return None;
    }
    let message = data["error"]
        .as_str()
        .map(str::trim)
        .filter(|message| !message.is_empty());
    Some(message.unwrap_or("Firecrawl request failed").to_owned())
}

pub fn build_request(ctx: &BuildContext<'_>) -> BuiltSearchRequest {
    let headers = match ctx.api_key {
        Some(api_key) => content_headers(BTreeMap::from([(
            "Authorization".into(),
            format!("Bearer {api_key}"),
        )])),
        None => content_headers(BTreeMap::new()),
    };
    BuiltSearchRequest {
        form: None,
        url: ctx.base_url.unwrap_or(DEFAULT_URL).into(),
        method: "POST",
        headers,
        body: Some(json!({
            "query": append_domain_filters(ctx.query, ctx.allowed_domains, ctx.blocked_domains),
            "limit": clamp(ctx.max_results, 1.0, 100.0),
            "sources": [{"type": "web"}],
        })),
    }
}

pub fn normalize_response(data: &Value) -> Vec<SearchResultItem> {
    collect(web_results(data).into_iter().map(|item| {
        let url = item["url"].as_str()?;
        let title = item["title"].as_str().unwrap_or(url);
        let snippet = item["description"]
            .as_str()
            .or_else(|| item["snippet"].as_str())
            .or_else(|| item["markdown"].as_str());
        result(Some(title), Some(url), snippet, None, None)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(api_key: Option<&'a str>, max: f64) -> BuildContext<'a> {
        BuildContext {
            query: "rust async",
            max_results: max,
            api_key,
            base_url: None,
            allowed_domains: None,
            blocked_domains: None,
        }
    }

    #[test]
    fn sends_bearer_key_and_clamps_the_limit() {
        let request = build_request(&ctx(Some("secret"), 500.0));
        assert_eq!(request.url, DEFAULT_URL);
        assert_eq!(request.method, "POST");
        assert_eq!(request.headers["Authorization"], "Bearer secret");
        assert_eq!(
            request.body.unwrap(),
            json!({"query":"rust async","limit":100.0,"sources":[{"type":"web"}]})
        );
    }

    #[test]
    fn omits_authorization_without_a_key() {
        assert!(
            !build_request(&ctx(None, 5.0))
                .headers
                .contains_key("Authorization")
        );
    }

    #[test]
    fn folds_domain_filters_into_the_query() {
        let allowed = vec!["docs.rs".to_string()];
        let request = build_request(&BuildContext {
            query: "serde",
            max_results: 3.0,
            api_key: None,
            base_url: None,
            allowed_domains: Some(&allowed),
            blocked_domains: None,
        });
        assert_eq!(request.body.unwrap()["query"], "serde site:docs.rs");
    }

    #[test]
    fn prefers_nested_data_web_and_snippet_fallbacks() {
        let results = normalize_response(&json!({"data":{"web":[
            {"url":"https://a.test","title":"A","description":"one"},
            {"url":"https://b.test","snippet":"two"},
            {"url":"https://c.test","markdown":"three"}
        ]}}));
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].title, "A");
        assert_eq!(results[0].snippet.as_deref(), Some("one"));
        assert_eq!(results[1].title, "https://b.test");
        assert_eq!(results[1].snippet.as_deref(), Some("two"));
        assert_eq!(results[2].snippet.as_deref(), Some("three"));
    }

    #[test]
    fn falls_back_to_top_level_results_and_skips_url_less_rows() {
        let results = normalize_response(
            &json!({"results":[{"title":"T","url":"https://a.test"},{"title":"x"}]}),
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://a.test");
    }

    #[test]
    fn ignores_primitive_payloads() {
        assert!(normalize_response(&json!(true)).is_empty());
    }

    #[test]
    fn reports_only_a_success_false_payload() {
        assert_eq!(response_error(&json!({"success":true})), None);
        assert_eq!(response_error(&json!({})), None);
        assert_eq!(
            response_error(&json!({"success":false})).unwrap(),
            "Firecrawl request failed"
        );
        assert_eq!(
            response_error(&json!({"success":false,"error":"  boom  "})).unwrap(),
            "boom"
        );
    }
}
