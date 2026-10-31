//! Port of senpi `websearch/websearch/providers/startpage.ts` (pin 903b8336).
//!
//! The upstream `prepareRequest` handshake (open the homepage, then post its search form) is split
//! into `prepare_request`, which takes the fetched homepage, so the transport stays with the caller.
use crate::websearch::html_document::{parse_html_document, query_selector, text_content};
use crate::websearch::providers::shared::{
    BuildContext, BuiltSearchRequest, FetchedPage, SearchResultItem, append_domain_filters,
    browser_headers, clean_text, collect, external_result_url, result, unique_by_url,
};
use regex::Regex;
use std::collections::BTreeMap;

pub const DEFAULT_URL: &str = "https://www.startpage.com/sp/search";
pub const STARTPAGE_HOME_URL: &str = "https://www.startpage.com/";

/// Startpage fronts its pages with a proof-of-work interstitial (Anubis) and redirects a search
/// without a fresh form token to a CAPTCHA shell.
pub fn detect_challenge(page: &FetchedPage) -> Option<&'static str> {
    if page.body.contains(r#"id="anubis_challenge""#) || page.body.contains("anubis_version") {
        return Some("proof-of-work interstitial");
    }
    if Regex::new(r"/(?:errors|captcha)/").is_ok_and(|re| re.is_match(&page.url))
        || page.body.contains("component---src-pages-captcha")
        || page.body.contains("/sp/captcha")
    {
        return Some("CAPTCHA page");
    }
    None
}

/// The hidden inputs of the homepage search form, when it carries a session token (`sc`).
pub fn search_form_inputs(html: &str) -> Option<Vec<(String, String)>> {
    let document = parse_html_document(html);
    let forms = document.select(r#"form[action="/sp/search"]"#);
    let form = forms.nodes().first()?;
    let mut inputs: Vec<(String, String)> = Vec::new();
    for input in dom_query::Selection::from(*form)
        .select(r#"input[type="hidden"]"#)
        .nodes()
    {
        if let Some(name) = input.attr("name").filter(|name| !name.is_empty()) {
            let value = input
                .attr("value")
                .map(|value| value.to_string())
                .unwrap_or_default();
            match inputs.iter_mut().find(|(existing, _)| *existing == *name) {
                Some(entry) => entry.1 = value,
                None => inputs.push((name.to_string(), value)),
            }
        }
    }
    inputs
        .iter()
        .any(|(name, value)| name == "sc" && !value.is_empty())
        .then_some(inputs)
}

pub fn normalize_html(html: &str) -> Vec<SearchResultItem> {
    let document = parse_html_document(html);
    let blocks = document.select("div.result");
    unique_by_url(collect(blocks.nodes().iter().map(|block| {
        let anchor = query_selector(block, "a.result-link");
        let url = external_result_url(
            anchor.and_then(|anchor| anchor.attr("href")).as_deref(),
            STARTPAGE_HOME_URL,
            &["startpage.com"],
        );
        let title = anchor
            .as_ref()
            .and_then(|anchor| query_selector(anchor, "h2, h3"))
            .or(anchor);
        result(
            clean_text(text_content(title.as_ref()).as_deref()).as_deref(),
            url.as_deref(),
            clean_text(text_content(query_selector(block, "p.description").as_ref()).as_deref())
                .as_deref(),
            None,
            None,
        )
    })))
}

pub fn build_request(ctx: &BuildContext<'_>) -> Result<BuiltSearchRequest, url::ParseError> {
    let mut url = url::Url::parse(ctx.base_url.unwrap_or(DEFAULT_URL))?;
    url.query_pairs_mut().append_pair(
        "query",
        &append_domain_filters(ctx.query, ctx.allowed_domains, ctx.blocked_domains),
    );
    Ok(BuiltSearchRequest {
        form: None,
        url: url.into(),
        method: "GET",
        headers: browser_headers(Some(STARTPAGE_HOME_URL), BTreeMap::new()),
        body: None,
    })
}

/// The homepage request a browser opens first.
pub fn home_request() -> BuiltSearchRequest {
    BuiltSearchRequest {
        form: None,
        url: STARTPAGE_HOME_URL.into(),
        method: "GET",
        headers: browser_headers(None, BTreeMap::new()),
        body: None,
    }
}

/// The outcome of the homepage handshake (`prepareRequest`).
#[derive(Clone, Debug, PartialEq)]
pub enum Prepared {
    /// Send this request; `form` is the urlencoded body of a form post.
    Request {
        request: BuiltSearchRequest,
        form: Option<String>,
    },
    Challenge(&'static str),
}

/// Posts the homepage search form with its session token, or keeps the plain GET when the
/// homepage carries no form.
pub fn prepare_request(
    ctx: &BuildContext<'_>,
    built: BuiltSearchRequest,
    home: &FetchedPage,
) -> Prepared {
    if let Some(challenge) = detect_challenge(home) {
        return Prepared::Challenge(challenge);
    }
    let inputs = (200..300)
        .contains(&home.status)
        .then(|| search_form_inputs(&home.body))
        .flatten();
    let Some(mut inputs) = inputs else {
        return Prepared::Request {
            request: built,
            form: None,
        };
    };
    let query = append_domain_filters(ctx.query, ctx.allowed_domains, ctx.blocked_domains);
    match inputs.iter_mut().find(|(name, _)| name == "query") {
        Some(entry) => entry.1 = query,
        None => inputs.push(("query".into(), query)),
    }
    let form = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(&inputs)
        .finish();
    Prepared::Request {
        request: BuiltSearchRequest {
            form: Some(form.clone()),
            url: ctx.base_url.unwrap_or(DEFAULT_URL).into(),
            method: "POST",
            headers: browser_headers(
                Some(STARTPAGE_HOME_URL),
                BTreeMap::from([(
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                )]),
            ),
            body: None,
        },
        form: Some(form),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> BuildContext<'static> {
        BuildContext {
            query: "typescript satisfies operator",
            max_results: 10.0,
            api_key: None,
            base_url: None,
            allowed_domains: None,
            blocked_domains: None,
        }
    }

    fn page(body: &str) -> FetchedPage {
        FetchedPage {
            status: 200,
            url: STARTPAGE_HOME_URL.into(),
            body: body.into(),
        }
    }

    #[test]
    fn posts_the_homepage_form_with_its_session_token() {
        let built = build_request(&ctx()).unwrap();
        let Prepared::Request { request, form } = prepare_request(
            &ctx(),
            built,
            &page(include_str!(
                "../../../tests/fixtures/websearch/startpage-home.html"
            )),
        ) else {
            panic!("expected a form post");
        };
        assert_eq!(request.url, "https://www.startpage.com/sp/search");
        assert_eq!(request.method, "POST");
        let form: BTreeMap<String, String> = url::form_urlencoded::parse(form.unwrap().as_bytes())
            .into_owned()
            .collect();
        assert_eq!(form["sc"], "REDACTEDSCTOKEN");
        assert_eq!(form["query"], "typescript satisfies operator");
    }

    #[test]
    fn normalizes_a_recorded_results_page() {
        let results = normalize_html(include_str!(
            "../../../tests/fixtures/websearch/startpage-results.html"
        ));
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].title, "Documentation - TypeScript 4.9");
        assert_eq!(
            results[0].url,
            "https://www.typescriptlang.org/docs/handbook/release-notes/typescript-4-9.html"
        );
        assert!(results[0].snippet.as_deref().unwrap().contains("satisfies"));
        for item in &results {
            assert!(item.url.starts_with("https://") && !item.url.contains("startpage.com"));
        }
    }

    #[test]
    fn reports_the_proof_of_work_interstitial_and_stops_at_the_homepage() {
        let built = build_request(&ctx()).unwrap();
        assert_eq!(
            prepare_request(
                &ctx(),
                built,
                &page(include_str!(
                    "../../../tests/fixtures/websearch/startpage-anubis.html"
                ))
            ),
            Prepared::Challenge("proof-of-work interstitial")
        );
        let plain = build_request(&ctx()).unwrap();
        assert_eq!(
            prepare_request(&ctx(), plain.clone(), &page("<html></html>")),
            Prepared::Request {
                request: plain,
                form: None
            }
        );
    }
}
