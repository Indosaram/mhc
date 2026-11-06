use super::shared::{
    BuildContext, BuiltSearchRequest, FetchedPage, SearchResultItem, append_domain_filters,
    browser_headers, result,
};
use crate::websearch::provider_endpoints::{SearchProvider, provider_url};
use regex::Regex;
use serde_json::Value;
use std::{collections::BTreeMap, sync::LazyLock};
static LINKS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"<a(?-u:\b)[^>]*class="[^"]*result__a[^"]*"[^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>"#,
    )
    .expect("literal pattern")
});
static SNIPPETS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<a(?-u:\b)[^>]*class="[^"]*result__snippet[^"]*"[^>]*>([\s\S]*?)</a>"#)
        .expect("literal pattern")
});
static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").expect("literal pattern"));
static SPACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\x09-\x0d\x20\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]+").expect("literal pattern")
});
static HEX_REFERENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)&#x([0-9a-f]+);").expect("literal pattern"));
static DECIMAL_REFERENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&#(\d+);").expect("literal pattern"));
fn code_point(digits: &str, radix: u32, original: &str) -> String {
    u32::from_str_radix(digits, radix)
        .ok()
        .and_then(char::from_u32)
        .map_or_else(|| original.into(), String::from)
}
fn html_decode(value: &str) -> String {
    let value = HEX_REFERENCE.replace_all(value, |captures: &regex::Captures<'_>| {
        code_point(&captures[1], 16, &captures[0])
    });
    let value = DECIMAL_REFERENCE.replace_all(&value, |captures: &regex::Captures<'_>| {
        code_point(&captures[1], 10, &captures[0])
    });
    value
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}
fn strip_html(value: &str) -> String {
    html_decode(
        SPACE
            .replace_all(&TAGS.replace_all(value, ""), " ")
            .trim_matches(' '),
    )
}
/// The no-JS frontend is a POST form; sending it the way its own form does keeps the request browser-shaped.
pub fn build_request(ctx: &BuildContext<'_>) -> Result<BuiltSearchRequest, url::ParseError> {
    let query = append_domain_filters(ctx.query, ctx.allowed_domains, ctx.blocked_domains);
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("q", &query)
        .append_pair("kl", "us-en")
        .append_pair("b", "")
        .finish();
    Ok(BuiltSearchRequest {
        form: Some(form),
        url: provider_url(SearchProvider::DuckduckgoHtml, ctx.base_url).into(),
        method: "POST",
        headers: browser_headers(
            Some("https://html.duckduckgo.com/"),
            BTreeMap::from([(
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            )]),
        ),
        body: None,
    })
}
/// DuckDuckGo answers throttled clients with an "anomaly" page instead of results, with status 200 or 202,
/// so the body is the only reliable signal.
pub fn detect_challenge(page: &FetchedPage) -> Option<&'static str> {
    (page.body.contains("anomaly-modal") || page.body.contains("anomaly.js"))
        .then_some("anomaly page")
}
pub fn normalize_response(data: &Value) -> Vec<SearchResultItem> {
    let html = data.get("html").and_then(Value::as_str).unwrap_or_default();
    let snippets: Vec<_> = SNIPPETS
        .captures_iter(html)
        .map(|capture| strip_html(&capture[1]))
        .collect();
    LINKS
        .captures_iter(html)
        .enumerate()
        .filter_map(|(index, capture)| {
            let decoded = html_decode(&capture[1]);
            let absolute = if decoded.starts_with("//") {
                format!("https:{decoded}")
            } else {
                decoded
            };
            let parsed = url::Url::parse(&absolute).ok()?;
            let destination = parsed
                .query_pairs()
                .find(|(key, _)| key == "uddg")
                .map(|(_, value)| value.into_owned())
                .unwrap_or(absolute);
            result(
                Some(&strip_html(&capture[2])),
                Some(&destination),
                snippets.get(index).map(String::as_str),
                None,
                None,
            )
        })
        .take(50)
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn redirect_entities_and_snippet() {
        let results = normalize_response(
            &json!({"html":"<a class=\"result__a\" href=\"//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.test\">A &amp; <b>B</b></a><a class=\"result__snippet\">two   words</a>"}),
        );
        assert_eq!(results[0].title, "A & B");
        assert_eq!(results[0].url, "https://a.test");
        assert_eq!(results[0].snippet.as_deref(), Some("two words"));
    }
    #[test]
    fn invalid_links_are_discarded() {
        assert!(
            normalize_response(
                &json!({"html":"<a class=\"result__a\" href=\"/relative\">Title</a>"})
            )
            .is_empty()
        );
    }
    fn page(body: &str, status: u16) -> FetchedPage {
        FetchedPage {
            status,
            url: "https://engine.example/search".into(),
            body: body.into(),
        }
    }
    #[test]
    fn posts_the_no_js_search_form_with_browser_navigation_headers() {
        let allowed = vec!["docs.example".into()];
        let built = build_request(&BuildContext {
            query: "typescript satisfies operator",
            max_results: 10.,
            api_key: None,
            base_url: None,
            allowed_domains: Some(&allowed),
            blocked_domains: None,
        })
        .unwrap();
        assert_eq!(built.url, "https://html.duckduckgo.com/html/");
        assert_eq!(built.method, "POST");
        assert!(built.body.is_none());
        assert_eq!(
            built.headers["Content-Type"],
            "application/x-www-form-urlencoded"
        );
        assert!(built.headers["User-Agent"].contains("Chrome/"));
        assert_eq!(built.headers["Sec-Fetch-Mode"], "navigate");
        let form = built.form.expect("form body");
        assert_eq!(
            url::form_urlencoded::parse(form.as_bytes())
                .find(|(key, _)| key == "q")
                .map(|(_, value)| value.into_owned())
                .as_deref(),
            Some("typescript satisfies operator site:docs.example")
        );
    }
    #[test]
    fn normalizes_the_recorded_results_page_and_decodes_numeric_references() {
        let results = normalize_response(
            &json!({"html":include_str!("../../../tests/fixtures/websearch/duckduckgo-results.html")}),
        );
        assert_eq!(results.len(), 3);
        assert_eq!(
            results[0].url,
            "https://www.typescriptlang.org/docs/handbook/release-notes/typescript-4-9.html"
        );
        assert_eq!(results[0].title, "Documentation - TypeScript 4.9");
        assert!(
            results[0]
                .snippet
                .as_deref()
                .unwrap_or_default()
                .contains("satisfies")
        );
        assert!(
            results[1]
                .snippet
                .as_deref()
                .unwrap_or_default()
                .contains("TypeScript's new `satisfies` operator")
        );
        let reference = Regex::new(r"(?i)&#x?[0-9a-f]+;").unwrap();
        for item in &results {
            assert!(!item.url.contains("duckduckgo.com"));
            assert!(!reference.is_match(&format!(
                "{} {}",
                item.title,
                item.snippet.as_deref().unwrap_or_default()
            )));
        }
    }
    #[test]
    fn reports_the_recorded_anomaly_page_as_a_challenge() {
        assert_eq!(
            detect_challenge(&page(
                include_str!("../../../tests/fixtures/websearch/duckduckgo-anomaly.html"),
                202
            )),
            Some("anomaly page")
        );
        assert_eq!(
            detect_challenge(&page(
                include_str!("../../../tests/fixtures/websearch/duckduckgo-results.html"),
                200
            )),
            None
        );
    }
}
