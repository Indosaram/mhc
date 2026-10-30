//! Port of senpi `websearch/websearch/providers/kagi.ts` (pin 903b8336).
use crate::websearch::providers::shared::{
    BuildContext, BuiltSearchRequest, SearchResultItem, clamp, collect, content_headers,
    json_count, result,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const DEFAULT_URL: &str = "https://kagi.com/api/v1/search";

pub fn build_request(ctx: &BuildContext<'_>) -> BuiltSearchRequest {
    let mut body =
        json!({"query": ctx.query, "limit": json_count(clamp(ctx.max_results, 1.0, 20.0))});
    if ctx.allowed_domains.is_some() || ctx.blocked_domains.is_some() {
        let mut lens = serde_json::Map::new();
        if let Some(domains) = ctx.allowed_domains {
            lens.insert("sites_included".into(), json!(domains));
        }
        if let Some(domains) = ctx.blocked_domains {
            lens.insert("sites_excluded".into(), json!(domains));
        }
        body["lens"] = Value::Object(lens);
    }
    BuiltSearchRequest {
        form: None,
        url: ctx.base_url.unwrap_or(DEFAULT_URL).into(),
        method: "POST",
        headers: content_headers(BTreeMap::from([(
            "Authorization".into(),
            format!("Bearer {}", ctx.api_key.unwrap_or_default()),
        )])),
        body: Some(body),
    }
}

pub fn normalize_response(data: &Value) -> Vec<SearchResultItem> {
    let items = data["data"]["search"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    collect(items.iter().map(|item| {
        let mut normalized = result(
            item["title"].as_str(),
            item["url"].as_str(),
            item["snippet"].as_str(),
            None,
            None,
        )?;
        if let Some(time) = item["time"].as_str().filter(|time| !time.is_empty()) {
            normalized.published_at = Some(time.into());
        }
        Some(normalized)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(
        max: f64,
        allowed: Option<&'a [String]>,
        blocked: Option<&'a [String]>,
    ) -> BuildContext<'a> {
        BuildContext {
            query: "current docs",
            max_results: max,
            api_key: Some("kagi-test"),
            base_url: None,
            allowed_domains: allowed,
            blocked_domains: blocked,
        }
    }

    #[test]
    fn posts_the_query_with_lens_domain_filters() {
        let allowed = vec!["api.example.com".to_string()];
        let request = build_request(&ctx(5.0, Some(&allowed), None));
        assert_eq!(request.url, "https://kagi.com/api/v1/search");
        assert_eq!(request.method, "POST");
        assert_eq!(request.headers["Authorization"], "Bearer kagi-test");
        assert_eq!(
            request.body.unwrap(),
            json!({"query":"current docs","limit":5.0,"lens":{"sites_included":["api.example.com"]}})
        );
    }

    #[test]
    fn maps_a_blocklist_into_lens_exclusions_and_omits_lens_without_filters() {
        let blocked = vec!["spam.example.com".to_string()];
        assert_eq!(
            build_request(&ctx(5.0, None, Some(&blocked))).body.unwrap(),
            json!({"query":"current docs","limit":5.0,"lens":{"sites_excluded":["spam.example.com"]}})
        );
        assert_eq!(
            build_request(&ctx(30.0, None, None)).body.unwrap(),
            json!({"query":"current docs","limit":20.0})
        );
    }

    #[test]
    fn normalizes_data_search_results_with_published_at() {
        let results = normalize_response(
            &json!({"meta":{"ms":42},"data":{"search":[{"title":"Kagi Result","url":"https://kagi.example.com","snippet":"Kagi snippet","time":"2026-01-01T00:00:00Z"}]}}),
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Kagi Result");
        assert_eq!(results[0].snippet.as_deref(), Some("Kagi snippet"));
        assert_eq!(
            results[0].published_at.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
    }
}
