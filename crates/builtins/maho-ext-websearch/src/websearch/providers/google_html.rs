//! Port of senpi `websearch/websearch/providers/google-html.ts` (pin 903b8336).
use crate::websearch::html_document::{closest, parse_html_document, query_selector, text_content};
use crate::websearch::providers::shared::{
    BuildContext, BuiltSearchRequest, FetchedPage, SearchResultItem, append_domain_filters,
    browser_headers, clamp, clean_text, collect, external_result_url, result, unique_by_url,
};
use regex::Regex;
use std::collections::BTreeMap;

pub const DEFAULT_URL: &str = "https://www.google.com/search";
const GOOGLE_HOME_URL: &str = "https://www.google.com/";
const SNIPPET_SELECTORS: &[&str] = &[".VwiC3b", ".IsZvec", ".BNeawe.s3v9rd", "[data-sncf='1']"];

/// Google serves a script-only wall to clients that do not run JavaScript, an "unsupported
/// browser" page to old user agents, and a `/sorry/` CAPTCHA to throttled ones.
pub fn detect_challenge(page: &FetchedPage) -> Option<&'static str> {
    let matches = |pattern: &str| Regex::new(pattern).is_ok_and(|re| re.is_match(&page.body));
    if page.url.contains("/sorry/") || matches(r"(?i)unusual traffic|g-recaptcha") {
        return Some("unusual-traffic CAPTCHA");
    }
    if matches(r"(?i)<h3\b") {
        return None;
    }
    if page.body.contains("/httpservice/retry/enablejs") {
        return Some("JavaScript wall");
    }
    if matches(r"(?i)browser isn.t supported anymore") {
        return Some("unsupported-browser page");
    }
    None
}

fn unwrap_google_url(href: Option<&str>) -> Option<String> {
    let url = url::Url::parse(GOOGLE_HOME_URL)
        .ok()?
        .join(href.filter(|href| !href.is_empty())?)
        .ok()?;
    let target = if url.path() == "/url" {
        let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        pairs
            .iter()
            .find(|(key, _)| key == "q")
            .or_else(|| pairs.iter().find(|(key, _)| key == "url"))
            .map(|(_, value)| value.clone())?
    } else {
        url.to_string()
    };
    external_result_url(Some(&target), GOOGLE_HOME_URL, &["google.com"])
}

pub fn normalize_html(html: &str) -> Vec<SearchResultItem> {
    let document = parse_html_document(html);
    let headings = document.select("a h3");
    unique_by_url(collect(headings.nodes().iter().map(|heading| {
        let container = closest(heading, ".tF2Cxc, .MjjYud, .Gx5Zad");
        let snippet = SNIPPET_SELECTORS.iter().find_map(|selector| {
            clean_text(
                text_content(
                    container
                        .as_ref()
                        .and_then(|container| query_selector(container, selector))
                        .as_ref(),
                )
                .as_deref(),
            )
        });
        let href = closest(heading, "a").and_then(|anchor| anchor.attr("href"));
        result(
            clean_text(Some(&heading.text())).as_deref(),
            unwrap_google_url(href.as_deref()).as_deref(),
            snippet.as_deref(),
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
        .append_pair(
            "num",
            &(clamp(ctx.max_results, 1.0, 20.0) as i64).to_string(),
        )
        .append_pair("hl", "en")
        .append_pair("udm", "14")
        .append_pair("pws", "0");
    Ok(BuiltSearchRequest {
        form: None,
        url: url.into(),
        method: "GET",
        headers: browser_headers(Some(GOOGLE_HOME_URL), BTreeMap::new()),
        body: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(body: &str, url: &str) -> FetchedPage {
        FetchedPage {
            status: 200,
            url: url.into(),
            body: body.into(),
        }
    }

    #[test]
    fn requests_the_plain_web_results_page() {
        let built = build_request(&BuildContext {
            query: "typescript satisfies operator",
            max_results: 10.0,
            api_key: None,
            base_url: None,
            allowed_domains: None,
            blocked_domains: None,
        })
        .unwrap();
        let url = url::Url::parse(&built.url).unwrap();
        assert_eq!(
            format!("{}{}", url.origin().ascii_serialization(), url.path()),
            "https://www.google.com/search"
        );
        let pairs: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["q"], "typescript satisfies operator");
        assert_eq!(pairs["udm"], "14");
        assert_eq!(pairs["num"], "10");
    }

    #[test]
    fn normalizes_a_recorded_results_page() {
        let results = normalize_html(include_str!(
            "../../../tests/fixtures/websearch/google-results.html"
        ));
        assert_eq!(results[0].title, "Documentation - TypeScript 4.9");
        assert_eq!(
            results[0].url,
            "https://www.typescriptlang.org/docs/handbook/release-notes/typescript-4-9.html"
        );
        assert!(results[0].snippet.as_deref().unwrap().contains("satisfies"));
        for item in &results {
            assert!(!item.url.contains("google.com"));
        }
    }

    #[test]
    fn reports_the_recorded_javascript_wall_as_a_challenge() {
        let results = include_str!("../../../tests/fixtures/websearch/google-results.html");
        assert_eq!(
            detect_challenge(&page(
                include_str!("../../../tests/fixtures/websearch/google-enablejs.html"),
                "https://engine.example/search"
            )),
            Some("JavaScript wall")
        );
        assert_eq!(
            detect_challenge(&page("<html></html>", "https://www.google.com/sorry/index")),
            Some("unusual-traffic CAPTCHA")
        );
        assert_eq!(
            detect_challenge(&page(results, "https://engine.example/search")),
            None
        );
    }

    #[test]
    fn unwraps_redirect_links() {
        assert_eq!(
            unwrap_google_url(Some("/url?q=https://docs.rs/x&sa=U")).as_deref(),
            Some("https://docs.rs/x")
        );
        assert_eq!(unwrap_google_url(Some("/search?q=next")), None);
    }
}
