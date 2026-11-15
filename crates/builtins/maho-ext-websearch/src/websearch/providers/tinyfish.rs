//! Port of senpi `websearch/websearch/providers/tinyfish.ts`.
use crate::websearch::providers::shared::{
    BuildContext, BuiltSearchRequest, SearchResultItem, clamp, clean_text, collect, result,
};
use serde_json::Value;
use std::collections::BTreeMap;

pub const DEFAULT_URL: &str = "https://api.search.tinyfish.ai";

pub const MAX_PAGE_RESULTS: f64 = 10.;
pub const MAX_PAGES: usize = 10;

struct Query<'a> {
    query: &'a str,
    count: f64,
    allowed_domains: Option<&'a [String]>,
    blocked_domains: Option<&'a [String]>,
}

fn tinyfish_request(ctx: &BuildContext<'_>, page: usize) -> BuiltSearchRequest {
    let query = Query {
        query: ctx.query,
        count: clamp(ctx.max_results, 1.0, 20.0),
        allowed_domains: ctx.allowed_domains,
        blocked_domains: ctx.blocked_domains,
    };
    build_page_request(ctx.base_url, ctx.api_key, &query, page)
}

fn build_page_request(
    base_url: Option<&str>,
    api_key: Option<&str>,
    query: &Query<'_>,
    page: usize,
) -> BuiltSearchRequest {
    let mut url = format!(
        "{}?query={}",
        base_url.unwrap_or(DEFAULT_URL),
        url_encode(query.query)
    );
    if let Some(allowed) = query.allowed_domains.filter(|d| !d.is_empty()) {
        url.push_str("&include_domains=");
        url.push_str(&url_encode(&allowed.join(",")));
    }
    if let Some(blocked) = query.blocked_domains.filter(|d| !d.is_empty()) {
        url.push_str("&exclude_domains=");
        url.push_str(&url_encode(&blocked.join(",")));
    }
    url.push_str("&num_results=");
    url.push_str(&(query.count.min(MAX_PAGE_RESULTS) as i64).to_string());
    url.push_str("&page=");
    url.push_str(&page.to_string());
    BuiltSearchRequest {
        form: None,
        url,
        method: "GET",
        headers: BTreeMap::from([
            ("Accept".into(), "application/json".into()),
            ("X-API-Key".into(), api_key.unwrap_or_default().into()),
        ]),
        body: None,
    }
}

fn url_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

pub fn build_request(ctx: &BuildContext<'_>) -> BuiltSearchRequest {
    tinyfish_request(ctx, 0)
}

pub fn next_page(
    ctx: &BuildContext<'_>,
    payload: &Value,
    page_index: usize,
) -> Option<BuiltSearchRequest> {
    let count = clamp(ctx.max_results, 1.0, 20.0);
    if page_index + 1 >= MAX_PAGES {
        return None;
    }
    let rows = payload["results"].as_array().map_or(0, Vec::len);
    if (rows as f64) < count.min(MAX_PAGE_RESULTS) {
        return None;
    }
    Some(tinyfish_request(ctx, page_index + 1))
}

pub fn normalize_response(data: &Value) -> Vec<SearchResultItem> {
    let items = data["results"].as_array().cloned().unwrap_or_default();
    collect(items.iter().map(|item| {
        let url = item["url"].as_str()?;
        let title = item["title"]
            .as_str()
            .or_else(|| item["site_name"].as_str())
            .unwrap_or(url);
        result(
            Some(title),
            Some(url),
            clean_text(item["snippet"].as_str()).as_deref(),
            None,
            None,
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx<'a>(
        allowed: Option<&'a [String]>,
        blocked: Option<&'a [String]>,
        max: f64,
    ) -> BuildContext<'a> {
        BuildContext {
            query: "rust async",
            max_results: max,
            api_key: Some("tf-key"),
            base_url: None,
            allowed_domains: allowed,
            blocked_domains: blocked,
        }
    }

    #[test]
    fn builds_the_first_page_with_domain_lists_and_api_key() {
        let allowed = vec!["a.test".to_string(), "b.test".to_string()];
        let request = build_request(&ctx(Some(&allowed), None, 4.0));
        assert_eq!(
            request.url,
            "https://api.search.tinyfish.ai?query=rust+async&include_domains=a.test%2Cb.test&num_results=4&page=0"
        );
        assert_eq!(request.method, "GET");
        assert_eq!(request.headers["X-API-Key"], "tf-key");
        assert!(request.body.is_none());
    }

    #[test]
    fn caps_a_page_at_ten_and_the_count_at_twenty() {
        let request = build_request(&ctx(None, None, 25.0));
        assert!(request.url.contains("num_results=10"));
        let blocked = vec!["c.test".to_string()];
        let request = build_request(&ctx(None, Some(&blocked), 3.0));
        assert!(request.url.contains("exclude_domains=c.test"));
    }

    #[test]
    fn paginates_only_while_a_full_page_comes_back() {
        let context = ctx(None, None, 15.0);
        let full = json!({"results": vec![json!({"url": "https://a.test"}); 10]});
        assert!(next_page(&context, &full, 0).is_some_and(|next| next.url.contains("page=1")));
        let short = json!({"results": [{"url": "https://a.test"}]});
        assert!(next_page(&context, &short, 0).is_none());
        assert!(next_page(&context, &full, MAX_PAGES - 1).is_none());
    }

    #[test]
    fn normalizes_titles_from_title_site_name_then_url() {
        let results = normalize_response(&json!({"results":[
            {"url":"https://a.test","title":"A","snippet":"  multi\n line "},
            {"url":"https://b.test","site_name":"B"},
            {"url":"https://c.test"},
            {"title":"no url"}
        ]}));
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].snippet.as_deref(), Some("multi line"));
        assert_eq!(results[1].title, "B");
        assert_eq!(results[2].title, "https://c.test");
    }
}
