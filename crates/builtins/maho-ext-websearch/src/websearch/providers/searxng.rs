//! Port of senpi `websearch/websearch/providers/searxng.ts` (pin 903b8336).
//! SearXNG is self-hosted: it has no default endpoint, so a request needs the configured base URL.
use crate::websearch::providers::shared::{
    BuildContext, BuiltSearchRequest, SearchResultItem, append_domain_filters, collect, result,
    unique_by_url,
};
use serde_json::Value;
use std::collections::BTreeMap;

fn searxng_search_url(base_url: &str) -> Result<url::Url, url::ParseError> {
    let mut url = url::Url::parse(base_url)?;
    let path = url.path().trim_end_matches('/');
    let path = path.strip_suffix("/search").unwrap_or(path);
    url.set_path(&format!("{path}/search"));
    Ok(url)
}

pub fn build_request(ctx: &BuildContext<'_>) -> Result<BuiltSearchRequest, url::ParseError> {
    let base = ctx.base_url.ok_or(url::ParseError::EmptyHost)?;
    let mut url = searxng_search_url(base)?;
    url.query_pairs_mut()
        .append_pair(
            "q",
            &append_domain_filters(ctx.query, ctx.allowed_domains, ctx.blocked_domains),
        )
        .append_pair("format", "json");
    Ok(BuiltSearchRequest {
        form: None,
        url: url.into(),
        method: "GET",
        headers: BTreeMap::from([("Accept".into(), "application/json".into())]),
        body: None,
    })
}

pub fn normalize_response(data: &Value) -> Vec<SearchResultItem> {
    let items = data["results"].as_array().cloned().unwrap_or_default();
    unique_by_url(collect(items.iter().map(|entry| {
        if !entry.is_object() {
            return None;
        }
        let url = entry["url"].as_str();
        let snippet = entry["content"]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty());
        let mut item = result(entry["title"].as_str().or(url), url, snippet, None, None)?;
        if let Some(published) = entry["publishedDate"]
            .as_str()
            .filter(|text| !text.is_empty())
        {
            item.published_at = Some(published.into());
        }
        Some(item)
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(base: &str) -> BuildContext<'_> {
        BuildContext {
            query: "typescript satisfies operator",
            max_results: 10.0,
            api_key: None,
            base_url: Some(base),
            allowed_domains: None,
            blocked_domains: None,
        }
    }

    #[test]
    fn requests_the_json_search_endpoint_under_the_instance_path() {
        assert_eq!(
            build_request(&ctx("http://localhost:8888/")).unwrap().url,
            "http://localhost:8888/search?q=typescript+satisfies+operator&format=json"
        );
        assert_eq!(
            build_request(&ctx("https://example.org/searx"))
                .unwrap()
                .url,
            "https://example.org/searx/search?q=typescript+satisfies+operator&format=json"
        );
        assert_eq!(
            build_request(&ctx("https://example.org/searx/search/"))
                .unwrap()
                .url,
            "https://example.org/searx/search?q=typescript+satisfies+operator&format=json"
        );
    }

    #[test]
    fn normalizes_a_recorded_json_answer() {
        let data: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/websearch/searxng-results.json"
        ))
        .unwrap();
        let results = normalize_response(&data);
        assert_eq!(results.len(), 3);
        assert_eq!(
            results[0].title,
            "TypeScript: Documentation - TypeScript 4.9"
        );
        assert_eq!(
            results[0].url,
            "https://www.typescriptlang.org/docs/handbook/release-notes/typescript-4-9.html"
        );
        assert!(results[0].snippet.as_deref().unwrap().contains("satisfies"));
    }
}
