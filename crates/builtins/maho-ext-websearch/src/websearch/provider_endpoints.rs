#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum SearchProvider { Exa,Tavily,Brave,DuckduckgoHtml,Deepseek,Serper,GoogleCse,Zai,Openai,Codex,Anthropic,Perplexity,Xai,Kimi }
impl SearchProvider { pub const fn as_str(self)->&'static str { match self { Self::Exa=>"exa",Self::Tavily=>"tavily",Self::Brave=>"brave",Self::DuckduckgoHtml=>"duckduckgo-html",Self::Deepseek=>"deepseek",Self::Serper=>"serper",Self::GoogleCse=>"google-cse",Self::Zai=>"z-ai",Self::Openai=>"openai",Self::Codex=>"codex",Self::Anthropic=>"anthropic",Self::Perplexity=>"perplexity",Self::Xai=>"xai",Self::Kimi=>"kimi" } } }
pub const fn default_provider_url(provider:SearchProvider)->&'static str {
    match provider {
        SearchProvider::Exa=>"https://api.exa.ai/search",
        SearchProvider::Tavily=>"https://api.tavily.com/search",
        SearchProvider::Brave=>"https://api.search.brave.com/res/v1/web/search",
        SearchProvider::DuckduckgoHtml=>"https://html.duckduckgo.com/html/",
        SearchProvider::Deepseek=>"https://api.deepseek.com/anthropic/v1/messages",
        SearchProvider::Serper=>"https://google.serper.dev/search",
        SearchProvider::GoogleCse=>"https://customsearch.googleapis.com/customsearch/v1",
        SearchProvider::Zai=>"https://api.z.ai/api/paas/v4/web_search",
        SearchProvider::Openai|SearchProvider::Codex=>"https://api.openai.com/v1/responses",
        SearchProvider::Anthropic=>"https://api.anthropic.com/v1/messages",
        SearchProvider::Perplexity=>"https://api.perplexity.ai/search",
        SearchProvider::Xai=>"https://api.x.ai/v1/responses",
        SearchProvider::Kimi=>"https://api.kimi.com/coding/v1/search",
    }
}
fn private_hostname(hostname:&str)->bool {
    let normalized=hostname.to_lowercase();
    let normalized=normalized.strip_prefix('[').unwrap_or(&normalized);
    let normalized=normalized.strip_suffix(']').unwrap_or(normalized);
    let normalized=normalized.strip_suffix('.').unwrap_or(normalized);
    if normalized=="localhost" || normalized.ends_with(".localhost") || normalized.contains(':') || normalized.starts_with("fc") || normalized.starts_with("fd") { return true; }
    if let Ok(ip)=normalized.parse::<std::net::Ipv4Addr>() {
        let [first,second,_,_]=ip.octets();
        return first==10 || first==127 || first==0 || (first==169 && second==254) || (first==172 && (16..=31).contains(&second)) || (first==192 && second==168);
    }
    false
}
pub fn is_allowed_provider_base_url(base_url:&str)->bool {
    let Ok(url)=url::Url::parse(base_url) else { return false; };
    url.scheme()=="https" && url.username().is_empty() && url.password().is_none() && url.host_str().is_some_and(|host|!host.ends_with("..") && !private_hostname(host))
}
pub fn provider_url(provider:SearchProvider,base_url:Option<&str>)->&str { base_url.unwrap_or(default_provider_url(provider)) }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn terminal_dot_private_hosts() { for url in ["https://localhost./search","https://sub.localhost./search","https://127.1../search","https://0177.0.0.1../search","https://2130706433../search","https://0x7f000001../search","https://10.1../search"] { assert!(!is_allowed_provider_base_url(url),"{url}"); } }
    #[test] fn public_single_terminal_dot() { assert!(is_allowed_provider_base_url("https://search-gateway.example.com./search")); }
    #[test] fn repeated_terminal_dots() { assert!(!is_allowed_provider_base_url("https://search-gateway.example.com../search")); }
    #[test] fn protocol_and_credentials() { for url in ["http://example.com","https://user:pass@example.com","invalid","https://[::1]/","https://192.168.1.1/","https://172.16.1.1/"] { assert!(!is_allowed_provider_base_url(url)); } }
    #[test] fn whatwg_short_ipv4() { assert!(!is_allowed_provider_base_url("https://127.1/")); assert!(!is_allowed_provider_base_url("https://0x7f000001/")); }
    #[test] fn override_and_default() { assert_eq!(provider_url(SearchProvider::Exa,None),"https://api.exa.ai/search"); assert_eq!(provider_url(SearchProvider::Exa,Some("https://example.com")),"https://example.com"); }
}
