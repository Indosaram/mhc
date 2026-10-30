//! Port of senpi `websearch/websearch/providers/serpdive.ts` (pin 903b8336).
use crate::websearch::providers::shared::{
    BuildContext, BuiltSearchRequest, SearchResultItem, append_domain_filters, clamp, collect,
    content_headers, json_count, result,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const DEFAULT_URL: &str = "https://api.serpdive.com/v1/search";

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
            "max_results": json_count(clamp(ctx.max_results, 1.0, 10.0)),
        })),
    }
}

pub fn normalize_response(data: &Value) -> Vec<SearchResultItem> {
    let items = data["results"].as_array().cloned().unwrap_or_default();
    collect(items.iter().map(|item| {
        result(
            item["title"].as_str(),
            item["url"].as_str(),
            item["content"].as_str(),
            None,
            None,
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sends_a_bearer_token_caps_results_and_folds_domain_filters() {
        let allowed = vec!["docs.example.com".to_string()];
        let blocked = vec!["reddit.com".to_string()];
        let request = build_request(&BuildContext {
            query: "current docs",
            max_results: 20.0,
            api_key: Some("sd-test"),
            base_url: None,
            allowed_domains: Some(&allowed),
            blocked_domains: Some(&blocked),
        });
        assert_eq!(request.url, "https://api.serpdive.com/v1/search");
        assert_eq!(request.headers["Authorization"], "Bearer sd-test");
        assert_eq!(
            request.body.unwrap(),
            json!({"query":"current docs site:docs.example.com -site:reddit.com","max_results":10.0})
        );
    }

    #[test]
    fn normalizes_results_content_into_the_snippet() {
        let results = normalize_response(
            &json!({"results":[{"title":"Docs","url":"https://docs.example.com","content":"Extracted page content","date":"2026-06-19"}],"response_time_ms":1400}),
        );
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].snippet.as_deref(),
            Some("Extracted page content")
        );
        assert_eq!(results[0].published_at, None);
    }
}
