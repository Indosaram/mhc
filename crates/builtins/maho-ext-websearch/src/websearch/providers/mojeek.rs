//! Port of senpi `websearch/websearch/providers/mojeek.ts` (pin 903b8336).
use crate::websearch::html_document::{parse_html_document, query_selector, text_content};
use crate::websearch::providers::shared::{
    BuildContext, BuiltSearchRequest, FetchedPage, SearchResultItem, append_domain_filters,
    browser_headers, clamp, clean_text, collect, external_result_url, result, unique_by_url,
};
use std::collections::BTreeMap;

pub const DEFAULT_URL: &str = "https://www.mojeek.com/search";
const MOJEEK_HOME_URL: &str = "https://www.mojeek.com/";
const MOJEEK_HOSTS: &[&str] = &["mojeek.com", "mojeek.co.uk", "mojeek.de", "mojeek.fr"];

/// Mojeek answers scripted clients with an ALTCHA proof-of-work page (200) or an
/// "automated queries" refusal (403).
pub fn detect_challenge(page: &FetchedPage) -> Option<&'static str> {
    if page.body.contains("results-standard") {
        return None;
    }
    if page.body.contains("captcha-wrap") || page.body.contains("altcha") {
        return Some("ALTCHA verification page");
    }
    if page
        .body
        .to_lowercase()
        .contains("sending automated queries")
    {
        return Some("automated-queries refusal");
    }
    None
}

pub fn normalize_html(html: &str) -> Vec<SearchResultItem> {
    let document = parse_html_document(html);
    let rows = document.select("ul.results-standard > li");
    unique_by_url(collect(rows.nodes().iter().map(|row| {
        let anchor = query_selector(row, "h2 a.title").or_else(|| query_selector(row, "a.title"));
        let url = external_result_url(
            anchor.and_then(|anchor| anchor.attr("href")).as_deref(),
            MOJEEK_HOME_URL,
            MOJEEK_HOSTS,
        );
        result(
            clean_text(text_content(anchor.as_ref()).as_deref()).as_deref(),
            url.as_deref(),
            clean_text(text_content(query_selector(row, "p.s").as_ref()).as_deref()).as_deref(),
            None,
            None,
        )
    })))
}

pub fn build_request(ctx: &BuildContext<'_>) -> Result<BuiltSearchRequest, url::ParseError> {
    let mut url = url::Url::parse(ctx.base_url.unwrap_or(DEFAULT_URL))?;
    url.query_pairs_mut()
        .append_pair(
            "q",
            &append_domain_filters(ctx.query, ctx.allowed_domains, ctx.blocked_domains),
        )
        .append_pair("t", &(clamp(ctx.max_results, 1.0, 20.0) as i64).to_string());
    Ok(BuiltSearchRequest {
        form: None,
        url: url.into(),
        method: "GET",
        headers: browser_headers(Some(MOJEEK_HOME_URL), BTreeMap::new()),
        body: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(body: &str) -> FetchedPage {
        FetchedPage {
            status: 200,
            url: "https://engine.example/search".into(),
            body: body.into(),
        }
    }

    #[test]
    fn requests_the_results_page_with_a_result_count() {
        let built = build_request(&BuildContext {
            query: "typescript satisfies operator",
            max_results: 50.0,
            api_key: None,
            base_url: None,
            allowed_domains: None,
            blocked_domains: None,
        })
        .unwrap();
        let url = url::Url::parse(&built.url).unwrap();
        assert_eq!(
            format!("{}{}", url.origin().ascii_serialization(), url.path()),
            "https://www.mojeek.com/search"
        );
        let pairs: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["q"], "typescript satisfies operator");
        assert_eq!(pairs["t"], "20");
        assert!(built.headers["User-Agent"].contains("Chrome/"));
    }

    #[test]
    fn normalizes_a_recorded_results_page() {
        let results = normalize_html(include_str!(
            "../../../tests/fixtures/websearch/mojeek-results.html"
        ));
        assert_eq!(results.len(), 3);
        assert_eq!(
            results[0].title,
            "typescript \u{2018}satisfies\u{2018} operator \u{b7} schpet\u{2019}s notebook"
        );
        assert_eq!(
            results[0].url,
            "https://schpet.com/linklog/typescript-satisfies-operator"
        );
        assert!(results[0].snippet.as_deref().unwrap().contains("satisfies"));
    }

    #[test]
    fn reports_the_recorded_verification_page_as_a_challenge() {
        assert_eq!(
            detect_challenge(&page(include_str!(
                "../../../tests/fixtures/websearch/mojeek-captcha.html"
            ))),
            Some("ALTCHA verification page")
        );
        assert_eq!(
            detect_challenge(&page(include_str!(
                "../../../tests/fixtures/websearch/mojeek-results.html"
            ))),
            None
        );
    }
}
