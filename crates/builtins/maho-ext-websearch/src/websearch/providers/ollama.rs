//! Port of senpi `websearch/websearch/providers/ollama.ts`.
use crate::websearch::providers::shared::{
    BuildContext, BuiltSearchRequest, SearchResultItem, append_domain_filters, clamp, collect,
    content_headers, result,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const DEFAULT_URL: &str = "https://ollama.com/api/web_search";

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
            "max_results": clamp(ctx.max_results, 1.0, 20.0),
        })),
    }
}

pub fn normalize_response(data: &Value) -> Vec<SearchResultItem> {
    let items = data["results"].as_array().cloned().unwrap_or_default();
    collect(items.iter().map(|item| {
        let url = item["url"].as_str()?;
        let title = item["title"].as_str().unwrap_or(url);
        let snippet = item["snippet"]
            .as_str()
            .or_else(|| item["content"].as_str());
        result(Some(title), Some(url), snippet, None, None)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(api_key: Option<&'a str>, max: f64) -> BuildContext<'a> {
        BuildContext {
            query: "local models",
            max_results: max,
            api_key,
            base_url: None,
            allowed_domains: None,
            blocked_domains: None,
        }
    }

    #[test]
    fn posts_query_and_clamped_count_with_bearer_auth() {
        let request = build_request(&ctx(Some("ollama-key"), 50.0));
        assert_eq!(request.url, DEFAULT_URL);
        assert_eq!(request.method, "POST");
        assert_eq!(request.headers["Authorization"], "Bearer ollama-key");
        assert_eq!(
            request.body.unwrap(),
            json!({"query":"local models","max_results":20.0})
        );
    }

    #[test]
    fn folds_domain_filters_into_the_query() {
        let blocked = vec!["spam.test".to_string()];
        let request = build_request(&BuildContext {
            query: "llm",
            max_results: 5.0,
            api_key: None,
            base_url: None,
            allowed_domains: None,
            blocked_domains: Some(&blocked),
        });
        assert_eq!(request.body.unwrap()["query"], "llm -site:spam.test");
    }

    #[test]
    fn normalizes_results_with_snippet_fallbacks() {
        let results = normalize_response(&json!({"results":[
            {"url":"https://a.test","title":"A","snippet":"s"},
            {"url":"https://b.test","content":"c"},
            {"title":"no url"}
        ]}));
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].snippet.as_deref(), Some("s"));
        assert_eq!(results[1].title, "https://b.test");
        assert_eq!(results[1].snippet.as_deref(), Some("c"));
    }

    #[test]
    fn ignores_primitive_payloads() {
        assert!(normalize_response(&json!(true)).is_empty());
    }
}
