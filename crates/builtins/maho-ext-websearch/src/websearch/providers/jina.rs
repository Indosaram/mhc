//! Port of senpi `websearch/websearch/providers/jina.ts`.
use crate::websearch::providers::shared::{
    BuildContext, BuiltSearchRequest, SearchResultItem, append_domain_filters, clamp, collect,
    result,
};
use serde_json::Value;
use std::collections::BTreeMap;

pub const DEFAULT_URL: &str = "https://s.jina.ai";

pub fn response_error(data: &Value) -> Option<String> {
    let code = data["code"].as_f64()?;
    (code != 200.0).then(|| format!("Jina API response reported failure ({code})"))
}

pub fn build_request(ctx: &BuildContext<'_>) -> BuiltSearchRequest {
    let site = ctx
        .allowed_domains
        .filter(|domains| domains.len() == 1)
        .and_then(|domains| domains[0].split('/').next());
    let query = append_domain_filters(
        ctx.query,
        if site.is_some() {
            None
        } else {
            ctx.allowed_domains
        },
        ctx.blocked_domains,
    );
    let root = ctx.base_url.unwrap_or(DEFAULT_URL).trim_end_matches('/');
    let mut url = format!("{root}/{}", url_encode(&query));
    url.push_str("?count=");
    url.push_str(&(clamp(ctx.max_results, 1.0, 20.0) as i64).to_string());
    let mut headers = BTreeMap::from([
        ("Accept".into(), "application/json".into()),
        (
            "Authorization".into(),
            format!("Bearer {}", ctx.api_key.unwrap_or_default()),
        ),
        ("X-Respond-With".into(), "no-content".into()),
        ("X-Retain-Images".into(), "none".into()),
    ]);
    if let Some(site) = site {
        headers.insert("X-Site".into(), site.to_owned());
    }
    BuiltSearchRequest {
        form: None,
        url,
        method: "GET",
        headers,
        body: None,
    }
}

fn url_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

pub fn normalize_response(data: &Value) -> Vec<SearchResultItem> {
    let items = data["data"].as_array().cloned().unwrap_or_default();
    collect(items.iter().map(|item| {
        let url = item["url"].as_str()?;
        let title = item["title"].as_str().unwrap_or(url);
        let snippet = item["description"]
            .as_str()
            .or_else(|| item["content"].as_str());
        result(Some(title), Some(url), snippet, None, None)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(
        allowed: Option<&'a [String]>,
        blocked: Option<&'a [String]>,
        max: f64,
    ) -> BuildContext<'a> {
        BuildContext {
            query: "rust async",
            max_results: max,
            api_key: Some("jina-key"),
            base_url: None,
            allowed_domains: allowed,
            blocked_domains: blocked,
        }
    }

    #[test]
    fn carries_the_query_in_the_path_and_keeps_a_single_domain_as_x_site() {
        let allowed = vec!["docs.rs/path".to_string()];
        let request = build_request(&ctx(Some(&allowed), None, 7.0));
        assert_eq!(request.url, "https://s.jina.ai/rust%20async?count=7");
        assert_eq!(request.method, "GET");
        assert_eq!(request.headers["X-Site"], "docs.rs");
        assert_eq!(request.headers["X-Respond-With"], "no-content");
        assert_eq!(request.headers["Authorization"], "Bearer jina-key");
        assert!(request.body.is_none());
    }

    #[test]
    fn folds_multiple_domains_into_the_query_and_clamps_the_count() {
        let allowed = vec!["a.test".to_string(), "b.test".to_string()];
        let blocked = vec!["c.test".to_string()];
        let request = build_request(&ctx(Some(&allowed), Some(&blocked), 99.0));
        assert_eq!(
            request.url,
            "https://s.jina.ai/rust%20async%20site%3Aa.test%20site%3Ab.test%20-site%3Ac.test?count=20"
        );
        assert!(!request.headers.contains_key("X-Site"));
    }

    #[test]
    fn reports_a_non_200_code_only() {
        assert_eq!(response_error(&serde_json::json!({"code":200})), None);
        assert_eq!(response_error(&serde_json::json!({})), None);
        assert_eq!(
            response_error(&serde_json::json!({"code":402})).unwrap(),
            "Jina API response reported failure (402)"
        );
    }

    #[test]
    fn maps_data_rows_and_prefers_description_over_content() {
        let results = normalize_response(&serde_json::json!({"data":[
            {"url":"https://a.test","title":"A","description":"d","content":"c"},
            {"url":"https://b.test","content":"only content"},
            {"title":"no url"}
        ]}));
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].snippet.as_deref(), Some("d"));
        assert_eq!(results[1].title, "https://b.test");
        assert_eq!(results[1].snippet.as_deref(), Some("only content"));
    }

    #[test]
    fn ignores_primitive_payloads() {
        assert!(normalize_response(&serde_json::json!(true)).is_empty());
    }
}
