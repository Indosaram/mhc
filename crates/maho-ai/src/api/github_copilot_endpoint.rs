//! Port of senpi packages/ai/src/api/github-copilot-endpoint.ts.
//!
//! The Copilot API host is per account plan (individual, business, enterprise, data-residency
//! tenants). A request sent to another plan's host is refused with `421 Misdirected Request`,
//! so the host always comes from the account's own token; the individual host is a last resort.

use serde_json::Value;

pub const GITHUB_COPILOT_INDIVIDUAL_BASE_URL: &str = "https://api.individual.githubcopilot.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubCopilotApiEndpoint {
    pub tid: String,
    pub url: String,
}

pub fn github_copilot_token_field(token: &str, key: &str) -> Option<String> {
    for part in token.split(';') {
        if let Some(separator) = part.find('=')
            && separator > 0
            && &part[..separator] == key
        {
            let value = &part[separator + 1..];
            return (!value.is_empty()).then(|| value.to_owned());
        }
    }
    None
}

pub fn github_copilot_base_url_from_token(token: &str) -> Option<String> {
    let proxy_host = github_copilot_token_field(token, "proxy-ep")?;
    let host = proxy_host
        .strip_prefix("proxy.")
        .map_or(proxy_host.clone(), |rest| format!("api.{rest}"));
    Some(format!("https://{host}"))
}

pub fn parse_github_copilot_api_endpoint(value: &Value) -> Option<String> {
    let url = url::Url::parse(value.as_str()?.trim()).ok()?;
    if url.scheme() != "https"
        || url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let origin = url.origin().ascii_serialization();
    Some(format!("{origin}{}", url.path().trim_end_matches('/')))
}

fn endpoint_for_token(endpoint: Option<&Value>, token: Option<&str>) -> Option<String> {
    let token = token.filter(|token| !token.is_empty())?;
    let endpoint = endpoint?.as_object()?;
    let tid = endpoint.get("tid")?.as_str()?;
    let url = parse_github_copilot_api_endpoint(endpoint.get("url")?)?;
    (Some(tid.to_owned()) == github_copilot_token_field(token, "tid")).then_some(url)
}

#[derive(Debug, Clone, Default)]
pub struct ResolveGitHubCopilotBaseUrlInput<'a> {
    pub token: Option<&'a str>,
    pub api_endpoint: Option<&'a Value>,
    pub enterprise_domain: Option<&'a str>,
}

/// The account's Copilot API base URL, in order: the token response's `endpoints.api` recorded for
/// this exact token (matched by `tid`, so a pooled sibling account never borrows it), the token's
/// `proxy-ep`, the GitHub Enterprise Server domain, then the individual host.
pub fn resolve_github_copilot_base_url(input: ResolveGitHubCopilotBaseUrlInput<'_>) -> String {
    let token = input.token.filter(|token| !token.is_empty());
    endpoint_for_token(input.api_endpoint, token)
        .or_else(|| token.and_then(github_copilot_base_url_from_token))
        .or_else(|| {
            input
                .enterprise_domain
                .filter(|domain| !domain.is_empty())
                .map(|domain| format!("https://copilot-api.{domain}"))
        })
        .unwrap_or_else(|| GITHUB_COPILOT_INDIVIDUAL_BASE_URL.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TOKEN: &str = "tid=abc;exp=1;proxy-ep=proxy.business.githubcopilot.com;sku=x";

    #[test]
    fn reads_token_fields_and_derives_the_proxy_host() {
        assert_eq!(
            github_copilot_token_field(TOKEN, "tid").as_deref(),
            Some("abc")
        );
        assert_eq!(github_copilot_token_field("tid=;x=1", "tid"), None);
        assert_eq!(github_copilot_token_field("=v;tid=1", ""), None);
        assert_eq!(
            github_copilot_base_url_from_token(TOKEN).as_deref(),
            Some("https://api.business.githubcopilot.com")
        );
    }

    #[test]
    fn parses_only_credential_free_https_endpoints() {
        assert_eq!(
            parse_github_copilot_api_endpoint(&json!(" https://api.x.test/v1/// ")).as_deref(),
            Some("https://api.x.test/v1")
        );
        assert_eq!(
            parse_github_copilot_api_endpoint(&json!("https://api.x.test/")).as_deref(),
            Some("https://api.x.test")
        );
        assert_eq!(
            parse_github_copilot_api_endpoint(&json!("http://api.x.test")),
            None
        );
        assert_eq!(
            parse_github_copilot_api_endpoint(&json!("https://u:p@api.x.test")),
            None
        );
        assert_eq!(parse_github_copilot_api_endpoint(&json!(3)), None);
    }

    #[test]
    fn resolves_in_endpoint_token_enterprise_individual_order() {
        let endpoint = json!({"tid": "abc", "url": "https://api.tenant.test"});
        let resolve = |token, api_endpoint, enterprise_domain| {
            resolve_github_copilot_base_url(ResolveGitHubCopilotBaseUrlInput {
                token,
                api_endpoint,
                enterprise_domain,
            })
        };
        assert_eq!(
            resolve(Some(TOKEN), Some(&endpoint), None),
            "https://api.tenant.test"
        );
        let sibling = json!({"tid": "other", "url": "https://api.tenant.test"});
        assert_eq!(
            resolve(Some(TOKEN), Some(&sibling), None),
            "https://api.business.githubcopilot.com"
        );
        assert_eq!(
            resolve(None, Some(&endpoint), Some("ghe.test")),
            "https://copilot-api.ghe.test"
        );
        assert_eq!(
            resolve(None, None, None),
            GITHUB_COPILOT_INDIVIDUAL_BASE_URL
        );
    }
}
