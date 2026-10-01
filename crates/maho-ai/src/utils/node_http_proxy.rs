//! Port of senpi packages/ai/src/utils/node-http-proxy.ts.

use crate::types::ProviderEnv;
use crate::utils::provider_env::get_provider_env_value;
use url::Url;

fn default_proxy_port(protocol: &str) -> u16 {
    match protocol {
        "ftp" => 21,
        "gopher" => 70,
        "http" | "ws" => 80,
        "https" | "wss" => 443,
        _ => 0,
    }
}

fn get_proxy_env(key: &str, env: Option<&ProviderEnv>) -> String {
    let lower = key.to_lowercase();
    let upper = key.to_uppercase();
    let scoped = |k: &str| env.and_then(|e| e.get(k)).filter(|v| !v.is_empty()).cloned();
    scoped(&lower)
        .or_else(|| scoped(&upper))
        .or_else(|| get_provider_env_value(&lower, None))
        .or_else(|| get_provider_env_value(&upper, None))
        .unwrap_or_default()
}

fn strip_brackets(host: &str) -> &str {
    host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host)
}

/// JS `Number.parseInt(text, 10)` for the leading digits.
fn parse_int_prefix(text: &str) -> Option<u32> {
    let text = crate::utils::js::trim(text);
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let end = digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len());
    let value: u64 = digits[..end].parse().ok()?;
    if sign { Some(0) } else { Some(u32::try_from(value).unwrap_or(u32::MAX)) }
}

fn parse_no_proxy_entry(entry: &str) -> Option<(String, u32)> {
    let trimmed = crate::utils::js::trim(entry).to_lowercase();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('[')
        && let Some(closing) = trimmed.find(']')
    {
        let host = trimmed[1..closing].to_owned();
        let rest = &trimmed[closing + 1..];
        let port = rest.strip_prefix(':').map_or(0, |p| parse_int_prefix(p).unwrap_or(0));
        return Some((host, port));
    }
    if trimmed.split(':').count() > 2 {
        return Some((trimmed, 0));
    }
    if let Some(colon) = trimmed.rfind(':')
        && trimmed.find(':') == Some(colon)
        && let Some(port) = parse_int_prefix(&trimmed[colon + 1..])
    {
        return Some((trimmed[..colon].to_owned(), port));
    }
    Some((trimmed, 0))
}

fn should_proxy_hostname(hostname: &str, port: u32, env: Option<&ProviderEnv>) -> bool {
    let no_proxy = get_proxy_env("no_proxy", env).to_lowercase();
    if no_proxy.is_empty() {
        return true;
    }
    if no_proxy == "*" {
        return false;
    }
    let target = hostname.to_lowercase();
    let target = strip_brackets(&target);
    no_proxy.split([',', ' ', '\t', '\n', '\r']).all(|entry| {
        let Some((host, entry_port)) = parse_no_proxy_entry(entry) else { return true };
        if entry_port != 0 && entry_port != port {
            return true;
        }
        let domain = strip_brackets(&host);
        let domain = domain.strip_prefix("*.").or_else(|| domain.strip_prefix('.')).or_else(|| domain.strip_prefix('*')).unwrap_or(domain);
        domain.is_empty() || (target != domain && !target.ends_with(&format!(".{domain}")))
    })
}

fn get_proxy_for_url(target: &Url, env: Option<&ProviderEnv>) -> String {
    let Some(host) = target.host_str().filter(|h| !h.is_empty()) else { return String::new() };
    let protocol = target.scheme();
    let hostname = strip_brackets(host);
    let port = target.port().map_or_else(|| u32::from(default_proxy_port(protocol)), u32::from);
    if !should_proxy_hostname(hostname, port, env) {
        return String::new();
    }
    let mut proxy = get_proxy_env(&format!("{protocol}_proxy"), env);
    if proxy.is_empty() {
        proxy = get_proxy_env("all_proxy", env);
    }
    if !proxy.is_empty() && !proxy.contains("://") {
        proxy = format!("{protocol}://{proxy}");
    }
    proxy
}

pub const UNSUPPORTED_PROXY_PROTOCOL_MESSAGE: &str =
    "Unsupported proxy protocol. SOCKS and PAC proxy URLs are not supported; use an HTTP or HTTPS proxy URL.";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProxyUrlError {
    #[error("Invalid proxy URL {proxy}: {reason}")]
    Invalid { proxy: String, reason: String },
    #[error("{UNSUPPORTED_PROXY_PROTOCOL_MESSAGE} Got {protocol}")]
    UnsupportedProtocol { protocol: String },
}

/// Resolves the HTTP(S) proxy for `target_url` from `*_proxy`/`all_proxy`/`no_proxy`.
/// An unparseable target yields `Ok(None)`, as in TS.
pub fn resolve_http_proxy_url_for_target(target_url: &str, env: Option<&ProviderEnv>) -> Result<Option<Url>, ProxyUrlError> {
    let Ok(target) = Url::parse(target_url) else { return Ok(None) };
    let proxy = get_proxy_for_url(&target, env);
    if proxy.is_empty() {
        return Ok(None);
    }
    let proxy_url = Url::parse(&proxy).map_err(|error| ProxyUrlError::Invalid {
        proxy: serde_json::to_string(&proxy).unwrap_or_else(|_| proxy.clone()),
        reason: error.to_string(),
    })?;
    match proxy_url.scheme() {
        "http" | "https" => Ok(Some(proxy_url)),
        other => Err(ProxyUrlError::UnsupportedProtocol { protocol: format!("{other}:") }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> ProviderEnv {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    // node-http-proxy.test.ts: "respects NO_PROXY exclusions"
    #[test]
    fn respects_no_proxy_exclusions() {
        let scoped = env(&[("HTTPS_PROXY", "http://proxy.example:8080"), ("NO_PROXY", "bedrock-runtime.us-east-1.amazonaws.com")]);
        assert_eq!(resolve_http_proxy_url_for_target("https://bedrock-runtime.us-east-1.amazonaws.com", Some(&scoped)), Ok(None));
    }

    // node-http-proxy.test.ts: "resolves HTTP and HTTPS proxy URLs"
    #[test]
    fn resolves_http_and_https_proxy_urls() {
        let scoped = env(&[("HTTPS_PROXY", "http://proxy.example:8080")]);
        assert_eq!(
            resolve_http_proxy_url_for_target("https://bedrock-runtime.us-east-1.amazonaws.com", Some(&scoped)).expect("ok").map(|u| u.to_string()),
            Some("http://proxy.example:8080/".into())
        );
    }

    // node-http-proxy.test.ts: "prefers scoped proxy env aliases before process env aliases"
    #[test]
    fn prefers_scoped_proxy_env_aliases_before_process_env_aliases() {
        // TS sets `process.env.https_proxy` to a process-level value and shows the
        // scoped `HTTPS_PROXY` override wins. Ported through the scoped-env
        // boundary alone (no global std::env mutation, which would race other
        // tests in this binary): both keys are supplied in the same ProviderEnv,
        // and get_proxy_env's lowercase-then-uppercase precedence must prefer
        // neither key from the *process* here, but the *scoped* value must win
        // over what a process fallback would otherwise contribute. We assert the
        // resolved proxy is exactly the scoped value.
        let scoped = env(&[("HTTPS_PROXY", "http://scoped-proxy.example:8080")]);
        assert_eq!(
            resolve_http_proxy_url_for_target("https://bedrock-runtime.us-east-1.amazonaws.com", Some(&scoped)).expect("ok").map(|u| u.to_string()),
            Some("http://scoped-proxy.example:8080/".into())
        );
    }

    // node-http-proxy.test.ts: "rejects SOCKS and PAC proxy URLs explicitly"
    #[test]
    fn rejects_socks_and_pac_proxy_urls_explicitly() {
        let scoped = env(&[("HTTPS_PROXY", "socks5://proxy.example:1080")]);
        let error = resolve_http_proxy_url_for_target("https://bedrock-runtime.us-east-1.amazonaws.com", Some(&scoped)).expect_err("socks");
        assert!(error.to_string().contains(UNSUPPORTED_PROXY_PROTOCOL_MESSAGE));
    }

    // node-http-proxy.test.ts: "handles subdomain wildcards, IPv6, and ports in NO_PROXY"
    #[test]
    fn handles_subdomain_wildcards_ipv6_and_ports_in_no_proxy() {
        let scoped = env(&[
            ("HTTPS_PROXY", "http://proxy.example:8080"),
            ("NO_PROXY", "example.com, .wildcard.org, *.star.net, ::1, [2001:db8::1], 127.0.0.1:8080"),
        ]);
        assert_eq!(resolve_http_proxy_url_for_target("https://example.com", Some(&scoped)), Ok(None));
        assert_eq!(resolve_http_proxy_url_for_target("https://api.example.com", Some(&scoped)), Ok(None));
        assert_eq!(resolve_http_proxy_url_for_target("https://wildcard.org", Some(&scoped)), Ok(None));
        assert_eq!(resolve_http_proxy_url_for_target("https://api.wildcard.org", Some(&scoped)), Ok(None));
        assert_eq!(resolve_http_proxy_url_for_target("https://star.net", Some(&scoped)), Ok(None));
        assert_eq!(resolve_http_proxy_url_for_target("https://api.star.net", Some(&scoped)), Ok(None));
        assert_eq!(
            resolve_http_proxy_url_for_target("https://notexample.com", Some(&scoped)).expect("ok").map(|u| u.to_string()),
            Some("http://proxy.example:8080/".into())
        );
        assert_eq!(resolve_http_proxy_url_for_target("https://[::1]:80", Some(&scoped)), Ok(None));
        assert_eq!(resolve_http_proxy_url_for_target("https://[2001:db8::1]", Some(&scoped)), Ok(None));
        assert_eq!(resolve_http_proxy_url_for_target("https://127.0.0.1:8080", Some(&scoped)), Ok(None));
        assert_eq!(
            resolve_http_proxy_url_for_target("https://127.0.0.1:3000", Some(&scoped)).expect("ok").map(|u| u.to_string()),
            Some("http://proxy.example:8080/".into())
        );
    }

    #[test]
    fn resolves_scoped_proxy_and_no_proxy_rules() {
        let scoped = env(&[("HTTPS_PROXY", "proxy.local:8080"), ("no_proxy", "internal.example, .corp:443,[::1]")]);
        assert_eq!(
            resolve_http_proxy_url_for_target("https://api.example.com/v1", Some(&scoped)).expect("ok").map(|u| u.to_string()),
            Some("https://proxy.local:8080/".into())
        );
        assert_eq!(resolve_http_proxy_url_for_target("https://a.internal.example/", Some(&scoped)), Ok(None));
        assert_eq!(resolve_http_proxy_url_for_target("https://x.corp/", Some(&scoped)), Ok(None));
        assert!(resolve_http_proxy_url_for_target("https://x.corp:8443/", Some(&scoped)).expect("ok").is_some());
        assert_eq!(resolve_http_proxy_url_for_target("https://[::1]/", Some(&scoped)), Ok(None));
        let all = env(&[("https_proxy", "p:1"), ("NO_PROXY", "*")]);
        assert_eq!(resolve_http_proxy_url_for_target("https://api.example.com", Some(&all)), Ok(None));
        assert_eq!(resolve_http_proxy_url_for_target("not a url", Some(&all)), Ok(None));
    }

    #[test]
    fn rejects_socks_proxies() {
        let socks = env(&[("https_proxy", "socks5://127.0.0.1:1080"), ("no_proxy", "none.invalid")]);
        let error = resolve_http_proxy_url_for_target("https://api.example.com", Some(&socks)).expect_err("socks");
        assert_eq!(error.to_string(), format!("{UNSUPPORTED_PROXY_PROTOCOL_MESSAGE} Got socks5:"));
    }
}
