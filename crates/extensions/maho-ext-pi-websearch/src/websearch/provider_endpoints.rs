pub use super::types::SearchProvider;
pub const fn default_provider_url(provider:SearchProvider)->&'static str{match provider{
    SearchProvider::Exa=>"https://api.exa.ai/search",
    SearchProvider::Tavily=>"https://api.tavily.com/search",
    SearchProvider::Brave=>"https://api.search.brave.com/res/v1/web/search",
    SearchProvider::DuckduckgoHtml=>"https://html.duckduckgo.com/html/",
    SearchProvider::Serper=>"https://google.serper.dev/search",
    SearchProvider::GoogleCse=>"https://customsearch.googleapis.com/customsearch/v1",
    SearchProvider::Zai=>"https://api.z.ai/api/paas/v4/web_search",
    SearchProvider::Openai|SearchProvider::Codex=>"https://api.openai.com/v1/responses",
    SearchProvider::Anthropic=>"https://api.anthropic.com/v1/messages",
    SearchProvider::Perplexity=>"https://api.perplexity.ai/search",
    SearchProvider::Xai=>"https://api.x.ai/v1/responses",
    SearchProvider::Kimi=>"https://api.kimi.com/coding/v1/search",
}}
pub fn provider_url(provider:SearchProvider,base_url:Option<&str>)->&str{base_url.unwrap_or_else(||default_provider_url(provider))}
pub fn is_allowed_provider_base_url(base_url:&str)->bool{
    let Ok(configured)=url::Url::parse(base_url)else{return false;};
    let Some(host)=configured.host_str()else{return false;};
    configured.scheme()=="https" && configured.username().is_empty() && configured.password().is_none() && !host.ends_with("..") && !private_hostname(host)
}
fn private_hostname(host:&str)->bool{
    let normalized=host.to_lowercase();let normalized=normalized.trim_start_matches('[').trim_end_matches(']').strip_suffix('.').unwrap_or(normalized.trim_start_matches('[').trim_end_matches(']'));
    normalized=="localhost"||normalized.ends_with(".localhost")||normalized.contains(':')||normalized.starts_with("fc")||normalized.starts_with("fd")||private_ipv4(normalized)
}
fn private_ipv4(host:&str)->bool{
    let parts:Vec<_>=host.split('.').map(|part|part.chars().take_while(char::is_ascii_digit).collect::<String>().parse::<u16>()).collect();
    if parts.len()!=4{return false;}
    let Ok(parts)=parts.into_iter().collect::<Result<Vec<_>,_>>()else{return false;};
    if parts.iter().any(|part|*part>255){return false;}
    matches!(parts[0],0|10|127)||(parts[0]==169&&parts[1]==254)||(parts[0]==172&&(16..=31).contains(&parts[1]))||(parts[0]==192&&parts[1]==168)
}
