use super::provider_endpoints::{SearchProvider,is_allowed_provider_base_url};
pub fn discovered_native_entry_id(provider:SearchProvider,route_key:&str)->String {
    use sha2::{Digest,Sha256};
    let digest=Sha256::digest(route_key.as_bytes());
    let fingerprint=digest[..8].iter().map(|byte|format!("{byte:02x}")).collect::<String>();
    format!("native-{}-{fingerprint}",provider.as_str())
}
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct NativeModelInfo { pub provider:String,pub id:String,pub base_url:String,pub api:Option<String> }
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct NativeProviderMapping { pub provider:SearchProvider,pub resource:&'static str,pub route_label:Option<String>,pub endpoint_path:Option<&'static str> }
pub fn native_mapping(model:&NativeModelInfo)->Option<NativeProviderMapping> {
    let openai=["gpt-4o","gpt-4.1","gpt-5"].iter().any(|prefix|model.id.starts_with(prefix)) && !model.id.contains("codex");
    let mapped=|provider,resource,route_label,endpoint_path|Some(NativeProviderMapping{provider,resource,route_label,endpoint_path});
    if model.provider=="openai" && openai { return mapped(SearchProvider::Openai,"responses",None,None); }
    if model.provider!="openai" && matches!(model.api.as_deref(),Some("openai-responses"|"azure-openai-responses")) && openai { return mapped(SearchProvider::Openai,"responses",Some(model.provider.clone()),None); }
    if model.provider=="anthropic" && model.id.starts_with("claude-") { return mapped(SearchProvider::Anthropic,"messages",None,None); }
    if model.provider!="anthropic" && model.api.as_deref()==Some("anthropic-messages") && model.id.starts_with("claude-") { return mapped(SearchProvider::Anthropic,"messages",Some(model.provider.clone()),None); }
    if model.provider=="deepseek" && model.id.starts_with("deepseek-v4-") { return mapped(SearchProvider::Deepseek,"messages",Some("deepseek".into()),Some("/anthropic/v1/messages")); }
    if model.provider=="xai" && model.id.starts_with("grok-") { return mapped(SearchProvider::Xai,"responses",None,None); }
    if model.provider=="perplexity" && model.id.starts_with("sonar") { return mapped(SearchProvider::Perplexity,"chat/completions",None,None); }
    if matches!(model.provider.as_str(),"z-ai"|"zai") && model.id.starts_with("glm-") { return mapped(SearchProvider::Zai,"chat/completions",None,None); }
    if model.provider=="kimi-coding" { return mapped(SearchProvider::Kimi,"search",None,None); }
    if model.provider=="openrouter" { let (provider,id)=model.id.split_once('/')?; if provider.is_empty() || provider=="openrouter" { return None; } return native_mapping(&NativeModelInfo{provider:provider.into(),id:id.into(),base_url:model.base_url.clone(),api:model.api.clone()}); }
    None
}
pub fn should_discover_native_route(active:Option<&NativeModelInfo>,available:&NativeModelInfo)->bool { native_mapping(available).is_some() && active.is_none_or(|active|active.provider==available.provider) }
pub fn build_endpoint_url(base_url:&str,resource:&str,endpoint_path:Option<&str>)->String {
    let Ok(mut url)=url::Url::parse(base_url) else { return base_url.into(); };
    if let Some(path)=endpoint_path { url.set_path(path); }
    else {
        let path=url.path().trim_end_matches('/'); let suffix=format!("/{resource}");
        let tail=path.rsplit('/').next().unwrap_or_default(); let version=tail.strip_prefix('v').is_some_and(|digits|!digits.is_empty() && digits.bytes().all(|byte|byte.is_ascii_digit()));
        let path=if path.ends_with(&suffix) { path.into() } else if version { format!("{path}{suffix}") } else { format!("{path}/v1{suffix}") }; url.set_path(&path);
    }
    url.set_fragment(None); url.into()
}
pub fn native_route_key(model:&NativeModelInfo)->Option<String> {
    let mapping=native_mapping(model)?; let endpoint=build_endpoint_url(&model.base_url,mapping.resource,mapping.endpoint_path);
    if !is_allowed_provider_base_url(&endpoint) { return None; }
    let mut url=url::Url::parse(&endpoint).ok()?; let host=url.host_str()?.strip_suffix('.').unwrap_or(url.host_str()?).to_owned(); url.set_host(Some(&host)).ok()?;
    Some(format!("{}|{url}",mapping.provider.as_str()))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn discovered_ids_use_first_sixteen_sha256_hex_characters() { assert_eq!(discovered_native_entry_id(SearchProvider::Openai,"openai|https://api.example.test/v1/responses"),"native-openai-9c901901d1756287"); }
    fn model(provider:&str,id:&str)->NativeModelInfo { NativeModelInfo{provider:provider.into(),id:id.into(),base_url:"https://api.example.test/v1/".into(),api:None} }
    #[test] fn openrouter_maps_effective_provider() { assert_eq!(native_mapping(&model("openrouter","anthropic/claude-sonnet-4")).unwrap().provider,SearchProvider::Anthropic); assert!(native_mapping(&model("openrouter","openrouter/x")).is_none()); }
    #[test] fn codex_model_is_not_discovered() { assert!(native_mapping(&model("openai","gpt-5-codex")).is_none()); assert!(native_mapping(&model("openai","gpt-5.5")).is_some()); }
    #[test] fn endpoints_preserve_query_and_drop_fragment() { assert_eq!(build_endpoint_url("https://example.test/v2/?a=1#fragment","responses",None),"https://example.test/v2/responses?a=1"); assert_eq!(build_endpoint_url("https://example.test/foo","messages",Some("/anthropic/v1/messages")),"https://example.test/anthropic/v1/messages"); }
    #[test] fn route_key_normalizes_terminal_dot_and_rejects_private_host() { let mut model=model("openai","gpt-5"); model.base_url="https://api.example.test./v1".into(); assert_eq!(native_route_key(&model).unwrap(),"openai|https://api.example.test/v1/responses"); model.base_url="https://localhost/v1".into(); assert!(native_route_key(&model).is_none()); }
}
