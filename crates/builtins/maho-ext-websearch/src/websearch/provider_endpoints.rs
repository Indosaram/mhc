#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchProvider {
    #[default]
    Exa,
    Tavily,
    Brave,
    DuckduckgoHtml,
    Deepseek,
    Serper,
    GoogleCse,
    Google,
    Zai,
    Openai,
    Codex,
    ChatgptSubscription,
    Anthropic,
    Perplexity,
    Xai,
    Kimi,
    Kagi,
    Startpage,
    Mojeek,
    Ecosia,
    GoogleHtml,
    Searxng,
    Serpdive,
    ExaMcp,
    Keenable,
    Firecrawl,
    Jina,
    Ollama,
    OpenRouter,
    Parallel,
    Public,
    Synthetic,
    Tinyfish,
}
impl SearchProvider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exa => "exa",
            Self::Tavily => "tavily",
            Self::Brave => "brave",
            Self::DuckduckgoHtml => "duckduckgo-html",
            Self::Deepseek => "deepseek",
            Self::Serper => "serper",
            Self::GoogleCse => "google-cse",
            Self::Google => "google",
            Self::Zai => "z-ai",
            Self::Openai => "openai",
            Self::Codex => "codex",
            Self::ChatgptSubscription => "chatgpt-subscription",
            Self::Anthropic => "anthropic",
            Self::Perplexity => "perplexity",
            Self::Xai => "xai",
            Self::Kimi => "kimi",
            Self::Kagi => "kagi",
            Self::Startpage => "startpage",
            Self::Mojeek => "mojeek",
            Self::Ecosia => "ecosia",
            Self::GoogleHtml => "google-html",
            Self::Searxng => "searxng",
            Self::Serpdive => "serpdive",
            Self::ExaMcp => "exa-mcp",
            Self::Keenable => "keenable",
            Self::Firecrawl => "firecrawl",
            Self::Jina => "jina",
            Self::Ollama => "ollama",
            Self::OpenRouter => "openrouter",
            Self::Parallel => "parallel",
            Self::Public => "public",
            Self::Synthetic => "synthetic",
            Self::Tinyfish => "tinyfish",
        }
    }
}
pub const fn default_provider_url(provider: SearchProvider) -> &'static str {
    match provider {
        SearchProvider::Exa => "https://api.exa.ai/search",
        SearchProvider::Tavily => "https://api.tavily.com/search",
        SearchProvider::Brave => "https://api.search.brave.com/res/v1/web/search",
        SearchProvider::DuckduckgoHtml => "https://html.duckduckgo.com/html/",
        SearchProvider::Deepseek => "https://api.deepseek.com/anthropic/v1/messages",
        SearchProvider::Serper => "https://google.serper.dev/search",
        SearchProvider::GoogleCse => "https://customsearch.googleapis.com/customsearch/v1",
        SearchProvider::Google => "https://generativelanguage.googleapis.com/v1beta",
        SearchProvider::Zai => "https://api.z.ai/api/paas/v4/web_search",
        SearchProvider::Openai | SearchProvider::Codex => "https://api.openai.com/v1/responses",
        SearchProvider::ChatgptSubscription => "https://chatgpt.com/backend-api/codex/responses",
        SearchProvider::Anthropic => "https://api.anthropic.com/v1/messages",
        SearchProvider::Perplexity => "https://api.perplexity.ai/search",
        SearchProvider::Xai => "https://api.x.ai/v1/responses",
        SearchProvider::Kimi => "https://api.kimi.com/coding/v1/search",
        SearchProvider::Kagi => "https://kagi.com/api/v1/search",
        SearchProvider::Startpage => "https://www.startpage.com/sp/search",
        SearchProvider::Mojeek => "https://www.mojeek.com/search",
        SearchProvider::Ecosia => "https://www.ecosia.org/search",
        SearchProvider::GoogleHtml => "https://www.google.com/search",
        SearchProvider::Searxng => "",
        SearchProvider::Serpdive => "https://api.serpdive.com/v1/search",
        SearchProvider::ExaMcp => "https://mcp.exa.ai/mcp",
        SearchProvider::Keenable => "https://api.keenable.ai/v1/search",
        SearchProvider::Firecrawl => "https://api.firecrawl.dev/v2/search",
        SearchProvider::Jina => "https://s.jina.ai",
        SearchProvider::Ollama => "https://ollama.com/api/web_search",
        SearchProvider::OpenRouter => "https://openrouter.ai/api/v1",
        SearchProvider::Parallel => "https://search.parallel.ai/mcp",
        SearchProvider::Public => "",
        SearchProvider::Synthetic => "https://api.synthetic.new/v2/search",
        SearchProvider::Tinyfish => "https://api.search.tinyfish.ai",
    }
}
fn private_hostname(hostname: &str) -> bool {
    let normalized = hostname.to_lowercase();
    let normalized = normalized.strip_prefix('[').unwrap_or(&normalized);
    let normalized = normalized.strip_suffix(']').unwrap_or(normalized);
    let normalized = normalized.strip_suffix('.').unwrap_or(normalized);
    if normalized == "localhost"
        || normalized.ends_with(".localhost")
        || normalized.contains(':')
        || normalized.starts_with("fc")
        || normalized.starts_with("fd")
    {
        return true;
    }
    if let Ok(ip) = normalized.parse::<std::net::Ipv4Addr>() {
        let [first, second, _, _] = ip.octets();
        return first == 10
            || first == 127
            || first == 0
            || (first == 169 && second == 254)
            || (first == 172 && (16..=31).contains(&second))
            || (first == 192 && second == 168);
    }
    false
}
pub fn is_allowed_provider_base_url(base_url: &str) -> bool {
    let Ok(url) = url::Url::parse(base_url) else {
        return false;
    };
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url
            .host_str()
            .is_some_and(|host| !host.ends_with("..") && !private_hostname(host))
}
fn is_private_ipv4(hostname: &str) -> bool {
    let parts = hostname.split('.').collect::<Vec<_>>();
    if parts.len() != 4 {
        return false;
    }
    let Some(octets) = parts
        .iter()
        .map(|part| {
            let digits = part.trim_start_matches(|c: char| c.is_ascii_whitespace());
            let end = digits
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(digits.len());
            digits[..end]
                .parse::<u16>()
                .ok()
                .filter(|value| *value <= 255)
        })
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    let (first, second) = (octets[0], octets[1]);
    first == 10
        || first == 127
        || first == 0
        || (first == 169 && second == 254)
        || (first == 172 && (16..=31).contains(&second))
        || (first == 192 && second == 168)
}
fn is_local_network_hostname(hostname: &str) -> bool {
    let normalized = hostname.to_lowercase();
    let normalized = normalized.strip_prefix('[').unwrap_or(&normalized);
    let normalized = normalized.strip_suffix(']').unwrap_or(normalized);
    let normalized = normalized.strip_suffix('.').unwrap_or(normalized);
    if normalized.contains(':') {
        let unique_local = normalized.split_once(':').is_some_and(|(head, _)| {
            (head.len() <= 4
                && (head.starts_with("fc") || head.starts_with("fd"))
                && head.bytes().skip(2).all(|byte| byte.is_ascii_hexdigit()))
                || (head.len() == 4
                    && head.starts_with("fe")
                    && matches!(head.as_bytes()[2], b'8' | b'9' | b'a' | b'b')
                    && head.as_bytes()[3].is_ascii_hexdigit())
        });
        return normalized == "::1" || unique_local;
    }
    normalized == "localhost"
        || normalized.ends_with(".localhost")
        || is_private_ipv4(normalized)
        || (!normalized.contains('.')
            && !normalized.is_empty()
            && normalized
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'))
        || [".local", ".lan", ".internal", ".home.arpa"]
            .iter()
            .any(|suffix| normalized.ends_with(suffix))
}
/// A SearXNG instance usually runs on the user's own machine or LAN, often without TLS. Plain http is allowed
/// only for local-network hosts; a public host must still use https, and credentials stay rejected.
pub fn is_allowed_searxng_base_url(base_url: &str) -> bool {
    let Ok(url) = url::Url::parse(base_url) else {
        return false;
    };
    let Some(host) = url.host_str().filter(|host| !host.is_empty()) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    match url.scheme() {
        "https" => true,
        "http" => is_local_network_hostname(host),
        _ => false,
    }
}
pub fn provider_url(provider: SearchProvider, base_url: Option<&str>) -> &str {
    base_url.unwrap_or(default_provider_url(provider))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_dot_private_hosts() {
        for url in [
            "https://localhost./search",
            "https://sub.localhost./search",
            "https://127.1../search",
            "https://0177.0.0.1../search",
            "https://2130706433../search",
            "https://0x7f000001../search",
            "https://10.1../search",
        ] {
            assert!(!is_allowed_provider_base_url(url), "{url}");
        }
    }
    #[test]
    fn public_single_terminal_dot() {
        assert!(is_allowed_provider_base_url(
            "https://search-gateway.example.com./search"
        ));
    }
    #[test]
    fn repeated_terminal_dots() {
        assert!(!is_allowed_provider_base_url(
            "https://search-gateway.example.com../search"
        ));
    }
    #[test]
    fn protocol_and_credentials() {
        for url in [
            "http://example.com",
            "https://user:pass@example.com",
            "invalid",
            "https://[::1]/",
            "https://192.168.1.1/",
            "https://172.16.1.1/",
        ] {
            assert!(!is_allowed_provider_base_url(url));
        }
    }
    #[test]
    fn searxng_allows_http_only_on_the_local_network() {
        for url in [
            "http://localhost:8888",
            "http://127.0.0.1:8080/",
            "http://192.168.1.20/",
            "http://10.0.0.5",
            "http://searx",
            "http://searx.local/",
            "http://searx.lan",
            "http://box.internal",
            "http://nas.home.arpa",
            "http://[::1]:8080",
            "http://[fd12::1]/",
            "https://search.example.com",
        ] {
            assert!(is_allowed_searxng_base_url(url), "{url}");
        }
        for url in [
            "http://search.example.com",
            "https://user:pass@search.example.com",
            "ftp://searx.local",
            "invalid",
            "http://8.8.8.8/",
        ] {
            assert!(!is_allowed_searxng_base_url(url), "{url}");
        }
    }
    #[test]
    fn whatwg_short_ipv4() {
        assert!(!is_allowed_provider_base_url("https://127.1/"));
        assert!(!is_allowed_provider_base_url("https://0x7f000001/"));
    }
    #[test]
    fn override_and_default() {
        assert_eq!(
            provider_url(SearchProvider::Exa, None),
            "https://api.exa.ai/search"
        );
        assert_eq!(
            provider_url(SearchProvider::Exa, Some("https://example.com")),
            "https://example.com"
        );
    }
}
