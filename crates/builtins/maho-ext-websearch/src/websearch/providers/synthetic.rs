//! Port of senpi `websearch/websearch/providers/synthetic.ts`.
use crate::websearch::providers::shared::{
    BuildContext, BuiltSearchRequest, SearchResultItem, append_domain_filters, collect,
    content_headers, result,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const DEFAULT_URL: &str = "https://api.synthetic.new/v2/search";

pub fn build_request(ctx: &BuildContext<'_>) -> BuiltSearchRequest {
    BuiltSearchRequest {
        form: None,
        url: ctx.base_url.unwrap_or(DEFAULT_URL).into(),
        method: "POST",
        headers: content_headers(BTreeMap::from([(
            "Authorization".into(),
            format!("Bearer {}", ctx.api_key.unwrap_or_default()),
        )])),
        body: Some(json!({
            "query": append_domain_filters(ctx.query, ctx.allowed_domains, ctx.blocked_domains),
        })),
    }
}

pub fn normalize_response(data: &Value) -> Vec<SearchResultItem> {
    let items = data["results"]
        .as_array()
        .or_else(|| data["data"].as_array())
        .cloned()
        .unwrap_or_default();
    collect(items.iter().map(|item| {
        let url = item["url"].as_str()?;
        let title = item["title"].as_str().unwrap_or(url);
        let snippet = item["text"]
            .as_str()
            .or_else(|| item["snippet"].as_str())
            .or_else(|| item["description"].as_str())
            .or_else(|| item["content"].as_str());
        let mut normalized = result(Some(title), Some(url), snippet, None, None)?;
        if let Some(published) = item["published"].as_str().filter(|text| !text.is_empty()) {
            normalized.published_at = Some(published.into());
        }
        Some(normalized)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(max: f64) -> BuildContext<'a> {
        BuildContext {
            query: "coding agents",
            max_results: max,
            api_key: Some("syn-key"),
            base_url: None,
            allowed_domains: None,
            blocked_domains: None,
        }
    }

    #[test]
    fn posts_only_the_query_and_keeps_its_generous_default() {
        let request = build_request(&ctx(10.0));
        assert_eq!(request.url, DEFAULT_URL);
        assert_eq!(request.method, "POST");
        assert_eq!(request.headers["Authorization"], "Bearer syn-key");
        assert_eq!(request.body.unwrap(), json!({"query":"coding agents"}));
    }

    #[test]
    fn folds_domain_filters_into_the_query() {
        let allowed = vec!["example.com".to_string()];
        let request = build_request(&BuildContext {
            query: "q",
            max_results: 5.0,
            api_key: None,
            base_url: None,
            allowed_domains: Some(&allowed),
            blocked_domains: None,
        });
        assert_eq!(request.body.unwrap()["query"], "q site:example.com");
    }

    #[test]
    fn normalizes_either_results_or_data_arrays() {
        let from_results =
            normalize_response(&json!({"results":[{"url":"https://a.test","title":"A"}]}));
        assert_eq!(from_results.len(), 1);
        assert_eq!(from_results[0].title, "A");
        let from_data =
            normalize_response(&json!({"data":[{"url":"https://b.test","description":"d"}]}));
        assert_eq!(from_data.len(), 1);
        assert_eq!(from_data[0].snippet.as_deref(), Some("d"));
    }

    #[test]
    fn ignores_primitive_payloads() {
        assert!(normalize_response(&json!(true)).is_empty());
    }
}
