//! Port of senpi `websearch/websearch/providers/keenable.ts` (pin 903b8336).
use crate::websearch::providers::shared::{
    BuildContext, BuiltSearchRequest, SearchResultItem, append_domain_filters, clamp, collect,
    content_headers, json_count, result,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const DEFAULT_URL: &str = "https://api.keenable.ai/v1/search";
/// Keyless twin of the default endpoint; identified by `X-Keenable-Title` instead of a key.
pub const PUBLIC_SEARCH_URL: &str = "https://api.keenable.ai/v1/search/public";

/// Accepts only absolute http(s) URLs; control characters would let text merge into the URL.
fn http_url(value: Option<&str>) -> Option<String> {
    let value = value.filter(|value| !value.is_empty() && value.len() <= 2048)?;
    if value.contains(['\t', '\n', '\r']) {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.into())
}

pub fn build_request(ctx: &BuildContext<'_>) -> BuiltSearchRequest {
    // A lone allowed domain maps to the native host-only `site` field.
    let site = ctx
        .allowed_domains
        .filter(|domains| domains.len() == 1)
        .map(|domains| domains[0].clone());
    let mut body = json!({
        "query": append_domain_filters(ctx.query, if site.is_some() { None } else { ctx.allowed_domains }, ctx.blocked_domains),
        "max_results": json_count(clamp(ctx.max_results, 1.0, 50.0)),
    });
    if let Some(site) = site {
        body["site"] = Value::String(site);
    }
    let keyed = ctx.api_key.is_some_and(|key| !key.is_empty());
    let url = if keyed || ctx.base_url.is_some() {
        ctx.base_url.unwrap_or(DEFAULT_URL)
    } else {
        PUBLIC_SEARCH_URL
    };
    let auth = if keyed {
        (
            "X-API-Key".to_string(),
            ctx.api_key.unwrap_or_default().to_string(),
        )
    } else {
        (
            "X-Keenable-Title".to_string(),
            maho_core::config::app_name(),
        )
    };
    BuiltSearchRequest {
        form: None,
        url: url.into(),
        method: "POST",
        headers: content_headers(BTreeMap::from([auth])),
        body: Some(body),
    }
}

pub fn normalize_response(data: &Value) -> Vec<SearchResultItem> {
    let items = data["results"].as_array().cloned().unwrap_or_default();
    collect(items.iter().map(|item| {
        if !item.is_object() {
            return None;
        }
        let url = http_url(item["url"].as_str())?;
        let snippet = item["snippet"]
            .as_str()
            .or_else(|| item["description"].as_str());
        let mut normalized = result(
            Some(item["title"].as_str().unwrap_or(&url)),
            Some(&url),
            snippet,
            None,
            None,
        )?;
        if let Some(published) = item["published_at"]
            .as_str()
            .filter(|text| !text.is_empty())
        {
            normalized.published_at = Some(published.into());
        }
        Some(normalized)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(
        key: Option<&'a str>,
        base: Option<&'a str>,
        max: f64,
        allowed: Option<&'a [String]>,
        blocked: Option<&'a [String]>,
    ) -> BuildContext<'a> {
        BuildContext {
            query: "current docs",
            max_results: max,
            api_key: key,
            base_url: base,
            allowed_domains: allowed,
            blocked_domains: blocked,
        }
    }

    #[test]
    fn posts_to_the_keyed_endpoint_with_x_api_key() {
        let request = build_request(&ctx(Some("kn-test"), None, 5.0, None, None));
        assert_eq!(request.url, "https://api.keenable.ai/v1/search");
        assert_eq!(request.method, "POST");
        assert_eq!(request.headers["X-API-Key"], "kn-test");
        assert!(!request.headers.contains_key("X-Keenable-Title"));
        assert_eq!(
            request.body.unwrap(),
            json!({"query":"current docs","max_results":5.0})
        );
    }

    #[test]
    fn posts_to_the_public_endpoint_with_an_app_title_without_a_key() {
        let request = build_request(&ctx(None, None, 5.0, None, None));
        assert_eq!(request.url, PUBLIC_SEARCH_URL);
        assert!(!request.headers.contains_key("X-API-Key"));
        assert!(!request.headers["X-Keenable-Title"].is_empty());
    }

    #[test]
    fn caps_max_results_and_honors_a_base_url_override() {
        assert_eq!(
            build_request(&ctx(None, None, 500.0, None, None))
                .body
                .unwrap()["max_results"],
            json!(50.0)
        );
        let proxied = build_request(&ctx(
            None,
            Some("https://search-gateway.example.com/v1/search"),
            5.0,
            None,
            None,
        ));
        assert_eq!(proxied.url, "https://search-gateway.example.com/v1/search");
        assert!(proxied.headers.contains_key("X-Keenable-Title"));
    }

    #[test]
    fn maps_a_lone_allowed_domain_to_site_and_folds_the_rest() {
        let one = vec!["docs.example.com".to_string()];
        assert_eq!(
            build_request(&ctx(None, None, 5.0, Some(&one), None))
                .body
                .unwrap(),
            json!({"query":"current docs","max_results":5.0,"site":"docs.example.com"})
        );
        let two = vec!["a.example".to_string(), "b.example".to_string()];
        assert_eq!(
            build_request(&ctx(None, None, 5.0, Some(&two), None))
                .body
                .unwrap()["query"],
            "current docs site:a.example site:b.example"
        );
        let blocked = vec!["spam.example.com".to_string()];
        assert_eq!(
            build_request(&ctx(None, None, 5.0, None, Some(&blocked)))
                .body
                .unwrap(),
            json!({"query":"current docs -site:spam.example.com","max_results":5.0})
        );
    }

    #[test]
    fn normalizes_results_with_snippet_fallback_and_published_at() {
        let results = normalize_response(&json!({"results":[
            {"title":"Docs","url":"https://docs.example.com","snippet":"S","published_at":"2026-01-01"},
            {"url":"https://untitled.example","description":"D"},
            {"title":"Bad","url":"javascript:alert(1)"},
            {"title":"Ctl","url":"https://x.example/\nevil"}
        ]}));
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].published_at.as_deref(), Some("2026-01-01"));
        assert_eq!(results[1].title, "https://untitled.example/");
        assert_eq!(results[1].snippet.as_deref(), Some("D"));
    }
}
