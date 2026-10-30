use serde_json::{Value, json};
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq)]
pub struct SearchResultItem {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
    pub source: Option<String>,
    pub score: Option<f64>,
    pub published_at: Option<String>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct BuiltSearchRequest {
    pub url: String,
    pub method: &'static str,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Value>,
    /// A url-encoded form body (`application/x-www-form-urlencoded`), sent when `body` is absent.
    pub form: Option<String>,
}
pub struct BuildContext<'a> {
    pub query: &'a str,
    pub max_results: f64,
    pub api_key: Option<&'a str>,
    pub base_url: Option<&'a str>,
    pub allowed_domains: Option<&'a [String]>,
    pub blocked_domains: Option<&'a [String]>,
}
pub fn content_headers(extra: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::from([
        ("Accept".into(), "application/json".into()),
        ("Content-Type".into(), "application/json".into()),
    ]);
    headers.extend(extra);
    headers
}
pub fn with_auth_headers(
    config: Option<&BTreeMap<String, String>>,
    headers: &mut BTreeMap<String, String>,
) {
    if let Some(config) = config {
        for (name, value) in config {
            let existing = headers
                .keys()
                .find(|key| key.eq_ignore_ascii_case(name))
                .cloned();
            if let Some(existing) = existing {
                headers.remove(&existing);
            }
            headers.insert(name.clone(), value.clone());
        }
    }
}
pub fn clamp(value: f64, min: f64, max: f64) -> f64 {
    if value.is_nan() || min.is_nan() || max.is_nan() {
        f64::NAN
    } else {
        value.trunc().min(max).max(min)
    }
}
pub fn append_domain_filters(
    query: &str,
    allowed: Option<&[String]>,
    blocked: Option<&[String]>,
) -> String {
    let mut parts = vec![query.to_owned()];
    for domain in allowed.unwrap_or_default() {
        parts.push(format!("site:{domain}"));
    }
    for domain in blocked.unwrap_or_default() {
        parts.push(format!("-site:{domain}"));
    }
    parts.join(" ")
}
pub fn unique(values: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    for value in values {
        if !result.contains(value) {
            result.push(value.clone());
        }
    }
    result
}
fn non_empty(values: Vec<String>) -> Vec<String> {
    if values.is_empty() {
        vec!["invalid.invalid".into()]
    } else {
        values
    }
}
pub fn resolve_domain_filters(
    config_allowed: Option<&[String]>,
    config_blocked: Option<&[String]>,
    request_allowed: Option<&[String]>,
    request_blocked: Option<&[String]>,
) -> (Option<Vec<String>>, Option<Vec<String>>) {
    if let Some(allowed) = config_allowed {
        let allowed: Vec<_> = allowed
            .iter()
            .filter(|d| request_allowed.is_none_or(|r| r.contains(d)))
            .filter(|d| request_blocked.is_none_or(|r| !r.contains(d)))
            .cloned()
            .collect();
        return (Some(non_empty(unique(&allowed))), None);
    }
    if let Some(blocked) = config_blocked {
        let blocked = unique(
            &blocked
                .iter()
                .chain(request_blocked.unwrap_or_default())
                .cloned()
                .collect::<Vec<_>>(),
        );
        if let Some(allowed) = request_allowed {
            return (
                Some(non_empty(
                    allowed
                        .iter()
                        .filter(|d| !blocked.contains(d))
                        .cloned()
                        .collect(),
                )),
                None,
            );
        }
        return (None, Some(blocked));
    }
    if let Some(allowed) = request_allowed {
        return (Some(unique(allowed)), None);
    }
    if let Some(blocked) = request_blocked {
        return (None, Some(unique(blocked)));
    }
    (None, None)
}
pub fn result(
    title: Option<&str>,
    url: Option<&str>,
    snippet: Option<&str>,
    source: Option<&str>,
    score: Option<f64>,
) -> Option<SearchResultItem> {
    let title = title.filter(|s| !s.is_empty())?;
    let url = url.filter(|s| !s.is_empty())?;
    Some(SearchResultItem {
        title: title.into(),
        url: url.into(),
        snippet: snippet.filter(|s| !s.is_empty()).map(str::to_owned),
        source: source.filter(|s| !s.is_empty()).map(str::to_owned),
        score,
        published_at: None,
    })
}
pub fn normalize_results(
    data: &Value,
    text_field: &str,
    fallback: Option<&str>,
) -> Vec<SearchResultItem> {
    data.get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            result(
                item.get("title").and_then(Value::as_str),
                item.get("url").and_then(Value::as_str),
                item.get(text_field)
                    .and_then(Value::as_str)
                    .or_else(|| fallback.and_then(|f| item.get(f).and_then(Value::as_str))),
                None,
                item.get("score").and_then(Value::as_f64),
            )
        })
        .take(50)
        .collect()
}
pub fn json_count(count: f64) -> Value {
    if count.is_finite() {
        json!(count)
    } else {
        Value::Null
    }
}
/// A response body as read, with the status and the final URL after redirects.
#[derive(Clone, Debug, PartialEq)]
pub struct FetchedPage {
    pub status: u16,
    pub url: String,
    pub body: String,
}
/// One stable desktop Chrome navigation fingerprint (`browserHeaders`).
pub fn browser_headers(
    referer: Option<&str>,
    extra: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut headers=BTreeMap::from([
        ("Accept".into(),"text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8".into()),
        ("Accept-Language".into(),"en-US,en;q=0.9".into()),
        ("User-Agent".into(),"Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36".into()),
        ("Sec-Fetch-Dest".into(),"document".into()),("Sec-Fetch-Mode".into(),"navigate".into()),
        ("Sec-Fetch-Site".into(),if referer.is_some() {"same-origin"} else {"none"}.into()),
        ("Sec-Fetch-User".into(),"?1".into()),("Upgrade-Insecure-Requests".into(),"1".into()),
    ]);
    if let Some(referer) = referer {
        headers.insert("Referer".into(), referer.into());
    }
    headers.extend(extra);
    headers
}
/// Collapses whitespace; `None` for a missing or blank value (`cleanText`).
pub fn clean_text(value: Option<&str>) -> Option<String> {
    let text = value?.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}
/// Accepts an http(s) result link that leaves the engine's own site (`externalResultUrl`).
pub fn external_result_url(href: Option<&str>, base: &str, own_hosts: &[&str]) -> Option<String> {
    let href = href.filter(|href| !href.is_empty())?;
    let url = url::Url::parse(base).ok()?.join(href).ok()?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    let host = url.host_str().unwrap_or_default().to_lowercase();
    if own_hosts
        .iter()
        .any(|own| host == *own || host.ends_with(&format!(".{own}")))
    {
        return None;
    }
    Some(url.into())
}
/// Keeps the first result for each URL (`uniqueByUrl`).
pub fn unique_by_url(items: Vec<SearchResultItem>) -> Vec<SearchResultItem> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.url.clone()))
        .collect()
}
/// `collect(items, 50)`.
pub fn collect(items: impl IntoIterator<Item = Option<SearchResultItem>>) -> Vec<SearchResultItem> {
    items.into_iter().flatten().take(50).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configured_allowlist_cannot_expand() {
        let a = vec!["a".into()];
        let b = vec!["b".into()];
        assert_eq!(
            resolve_domain_filters(Some(&a), None, Some(&b), None),
            (Some(vec!["invalid.invalid".into()]), None)
        );
    }
    #[test]
    fn configured_blocklist_narrows_requested_allowlist() {
        let blocked = vec!["a".into()];
        let allowed = vec!["a".into(), "b".into()];
        assert_eq!(
            resolve_domain_filters(None, Some(&blocked), Some(&allowed), None),
            (Some(vec!["b".into()]), None)
        );
    }
    #[test]
    fn empty_request_allowlist_stays_empty() {
        assert_eq!(
            resolve_domain_filters(None, None, Some(&[]), None),
            (Some(vec![]), None)
        );
    }
    #[test]
    fn preserves_unique_order() {
        assert_eq!(unique(&["b".into(), "a".into(), "b".into()]), ["b", "a"]);
    }
    #[test]
    fn domain_query_filters() {
        assert_eq!(
            append_domain_filters("q", Some(&["a".into()]), Some(&["b".into()])),
            "q site:a -site:b"
        );
    }
    #[test]
    fn configured_headers_merge_case_insensitively() {
        let config = BTreeMap::from([("X-Account".into(), "one".into())]);
        let mut request = BTreeMap::from([
            ("x-account".into(), "catalog".into()),
            ("Authorization".into(), "Bearer key".into()),
        ]);
        with_auth_headers(Some(&config), &mut request);
        assert_eq!(request.get("X-Account").map(String::as_str), Some("one"));
        assert_eq!(
            request.get("Authorization").map(String::as_str),
            Some("Bearer key")
        );
        assert_eq!(request.len(), 2);
    }
    #[test]
    fn clean_text_collapses_and_drops_blank() {
        assert_eq!(clean_text(Some("  a\n b ")).as_deref(), Some("a b"));
        assert_eq!(clean_text(Some(" \t")), None);
        assert_eq!(clean_text(None), None);
    }
    #[test]
    fn external_result_url_rejects_own_hosts_and_non_http() {
        assert_eq!(
            external_result_url(Some("/x"), "https://www.ecosia.org/", &["ecosia.org"]),
            None
        );
        assert_eq!(
            external_result_url(Some("javascript:alert(1)"), "https://e.org/", &[]),
            None
        );
        assert_eq!(
            external_result_url(Some("https://docs.rs/a"), "https://e.org/", &["e.org"]).as_deref(),
            Some("https://docs.rs/a")
        );
    }
    #[test]
    fn browser_headers_carry_referer() {
        let headers = browser_headers(Some("https://r/"), BTreeMap::new());
        assert_eq!(headers["Sec-Fetch-Site"], "same-origin");
        assert_eq!(headers["Referer"], "https://r/");
        assert_eq!(
            browser_headers(None, BTreeMap::new())["Sec-Fetch-Site"],
            "none"
        );
    }
    #[test]
    fn invalid_results_dropped() {
        assert!(result(Some(""), Some("url"), None, None, None).is_none());
        assert!(result(Some("title"), None, None, None, None).is_none());
    }
}
