//! Port of senpi `websearch/websearch/providers/ecosia.ts` (pin 903b8336).
use crate::websearch::html_document::{closest, parse_html_document, query_selector, text_content};
use crate::websearch::providers::shared::{
    BuildContext, BuiltSearchRequest, FetchedPage, SearchResultItem, append_domain_filters,
    browser_headers, clean_text, collect, external_result_url, result, unique_by_url,
};
use std::collections::BTreeMap;

pub const DEFAULT_URL: &str = "https://www.ecosia.org/search";
const ECOSIA_HOME_URL: &str = "https://www.ecosia.org/";

/// Ecosia's firewall serves a managed challenge (usually HTTP 403) titled "Ecosia Firewall".
pub fn detect_challenge(page: &FetchedPage) -> Option<&'static str> {
    (page.body.contains("Ecosia Firewall")
        || page.body.contains("_cf_chl_opt")
        || page.body.contains("/cdn-cgi/challenge-platform/"))
    .then_some("firewall challenge")
}

pub fn normalize_html(html: &str) -> Vec<SearchResultItem> {
    let document = parse_html_document(html);
    let articles = document.select(r#"article[data-test-id="organic-result"]"#);
    unique_by_url(collect(articles.nodes().iter().map(|article| {
        let heading = query_selector(article, r#"[data-test-id="result-title"]"#);
        let link = heading.as_ref().and_then(|heading| closest(heading, "a"));
        let url = external_result_url(
            link.and_then(|link| link.attr("href")).as_deref(),
            ECOSIA_HOME_URL,
            &["ecosia.org"],
        );
        let description = query_selector(article, r#"[data-test-id="web-result-description"]"#)
            .or_else(|| query_selector(article, r#"[data-test-id="result-description"]"#));
        result(
            clean_text(text_content(heading.as_ref()).as_deref()).as_deref(),
            url.as_deref(),
            clean_text(text_content(description.as_ref()).as_deref()).as_deref(),
            None,
            None,
        )
    })))
}

pub fn build_request(ctx: &BuildContext<'_>) -> Result<BuiltSearchRequest, url::ParseError> {
    let mut url = url::Url::parse(ctx.base_url.unwrap_or(DEFAULT_URL))?;
    url.query_pairs_mut().append_pair(
        "q",
        &append_domain_filters(ctx.query, ctx.allowed_domains, ctx.blocked_domains),
    );
    Ok(BuiltSearchRequest {
        form: None,
        url: url.into(),
        method: "GET",
        headers: browser_headers(Some(ECOSIA_HOME_URL), BTreeMap::new()),
        body: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_the_results_page() {
        let built = build_request(&BuildContext {
            query: "typescript satisfies operator",
            max_results: 10.0,
            api_key: None,
            base_url: None,
            allowed_domains: None,
            blocked_domains: None,
        })
        .unwrap();
        assert_eq!(
            built.url,
            "https://www.ecosia.org/search?q=typescript+satisfies+operator"
        );
    }

    #[test]
    fn normalizes_a_recorded_results_page() {
        let results = normalize_html(include_str!(
            "../../../tests/fixtures/websearch/ecosia-results.html"
        ));
        assert_eq!(results.len(), 3);
        for item in &results {
            assert!(!item.url.contains("ecosia.org"));
            assert!(!item.snippet.as_deref().unwrap_or_default().is_empty());
        }
    }

    #[test]
    fn reports_the_recorded_firewall_page_as_a_challenge() {
        let page = FetchedPage {
            status: 403,
            url: "https://www.ecosia.org/search".into(),
            body: include_str!("../../../tests/fixtures/websearch/ecosia-firewall.html").into(),
        };
        assert_eq!(detect_challenge(&page), Some("firewall challenge"));
    }
}
