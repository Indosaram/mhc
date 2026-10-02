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
pub async fn build_native_entry<F,Fut>(model:Option<&NativeModelInfo>,auth:Option<&F>,id:Option<&str>,signal:Option<&maho_tools::definition::AbortSignal>)->Result<Option<super::types::SearchProviderEntry>,maho_tools::definition::ToolError>
where F:Fn(NativeModelInfo)->Fut,Fut:std::future::Future<Output=Option<String>> {
    let (Some(model),Some(auth))=(model,auth) else {return Ok(None)};
    let Some(mapping)=native_mapping(model) else {return Ok(None)};
    let base_url=build_endpoint_url(&model.base_url,mapping.resource,mapping.endpoint_path);
    if !is_allowed_provider_base_url(&base_url) {return Ok(None)}
    if let Some(signal)=signal {signal.check()?;}
    let future=auth(model.clone());
    let key=if let Some(signal)=signal {signal.check()?;tokio::select!{biased; ()=signal.cancelled()=>return Err(maho_tools::definition::ToolError::Aborted),key=future=>key}} else {future.await};
    let Some(key)=key.filter(|key|!key.is_empty()) else {return Ok(None)};
    let mut config=super::types::SearchProviderConfig::new(mapping.provider);
    config.id=Some(id.map_or_else(||mapping.route_label.map_or_else(||"native".into(),|label|format!("{label}/native")),String::from));
    config.api_key=Some(key);config.base_url=Some(base_url);config.model=Some(model.id.clone());
    Ok(Some(super::types::SearchProviderEntry{config,priority:Some(-1.),weight:None}))
}
pub async fn build_native_entries<F,Fut>(model:Option<&NativeModelInfo>,available:Option<&[NativeModelInfo]>,auth:Option<&F>,signal:Option<&maho_tools::definition::AbortSignal>)->Result<Vec<super::types::SearchProviderEntry>,maho_tools::definition::ToolError>
where F:Fn(NativeModelInfo)->Fut,Fut:std::future::Future<Output=Option<String>> {
    if let Some(signal)=signal {signal.check()?;}
    if auth.is_none() {return Ok(vec![])}
    let mut entries=Vec::new();let mut seen=std::collections::BTreeSet::new();
    if let Some(key)=model.and_then(native_route_key) {
        seen.insert(key);
        if let Some(entry)=build_native_entry(model,auth,None,signal).await? {entries.push(entry);}
    }
    for available_model in available.unwrap_or_default() {
        if !should_discover_native_route(model,available_model) {continue}
        let Some(key)=native_route_key(available_model) else {continue};
        if !seen.insert(key.clone()) {continue}
        if let Some(mut entry)=build_native_entry(Some(available_model),auth,Some("native-discovered"),signal).await? {
            entry.config.id=Some(discovered_native_entry_id(entry.config.provider,&key));entries.push(entry);
        }
    }
    Ok(entries)
}
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
    #[tokio::test] async fn native_discovery_deduplicates_before_auth_including_failed_active_route() {
        let calls=std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));let recorded=calls.clone();
        let auth=move |model:NativeModelInfo| {recorded.lock().unwrap().push(model.id.clone());async move {if model.id=="gpt-5.5" {None} else {Some("fixture-key".into())}}};
        let active=model("openai","gpt-5.5");let aliases=[model("openai","gpt-4.1")];
        assert!(build_native_entries(Some(&active),Some(&aliases),Some(&auth),None).await.unwrap().is_empty());assert_eq!(*calls.lock().unwrap(),["gpt-5.5"]);
        calls.lock().unwrap().clear();
        let aliases=[model("anthropic","claude-opus-4"),model("anthropic","claude-opus-4-8"),model("z-ai","glm-4.6-0"),model("z-ai","glm-4.6-1")];
        let entries=build_native_entries(None,Some(&aliases),Some(&auth),None).await.unwrap();
        assert_eq!(entries.len(),2);assert_eq!(*calls.lock().unwrap(),["claude-opus-4","glm-4.6-0"]);
        assert!(entries.iter().all(|entry|entry.config.id.as_ref().unwrap().starts_with("native-")));assert_ne!(entries[0].config.id,entries[1].config.id);
    }
    #[tokio::test] async fn native_entry_preserves_source_fields_and_rejects_private_before_auth() {
        let auth=|_:NativeModelInfo|async {Some("fixture-key".into())};
        let entry=build_native_entry(Some(&model("deepseek","deepseek-v4-flash")),Some(&auth),None,None).await.unwrap().unwrap();
        assert_eq!(entry.config.id.as_deref(),Some("deepseek/native"));assert_eq!(entry.config.model.as_deref(),Some("deepseek-v4-flash"));assert_eq!(entry.priority,Some(-1.));
        assert_eq!(entry.config.base_url.as_deref(),Some("https://api.example.test/anthropic/v1/messages"));
        let mut private=model("openai","gpt-5");private.base_url="https://localhost./v1".into();
        let no_auth=|_:NativeModelInfo|async {panic!("private URL must not resolve auth")};
        assert!(build_native_entry(Some(&private),Some(&no_auth),None,None).await.unwrap().is_none());
    }
    #[tokio::test] async fn native_auth_cancellation_subscribes_before_resolution() {
        let signal=maho_tools::definition::AbortSignal::default();let trigger=signal.clone();
        let auth=move |_:NativeModelInfo| {let trigger=trigger.clone();async move {trigger.abort();std::future::pending::<Option<String>>().await}};
        assert!(matches!(tokio::time::timeout(std::time::Duration::from_secs(5),build_native_entry(Some(&model("openai","gpt-5")),Some(&auth),None,Some(&signal))).await.unwrap(),Err(maho_tools::definition::ToolError::Aborted)));
    }
    #[test] fn discovered_ids_use_first_sixteen_sha256_hex_characters() { assert_eq!(discovered_native_entry_id(SearchProvider::Openai,"openai|https://api.example.test/v1/responses"),"native-openai-9c901901d1756287"); }
    fn model(provider:&str,id:&str)->NativeModelInfo { NativeModelInfo{provider:provider.into(),id:id.into(),base_url:"https://api.example.test/v1/".into(),api:None} }
    #[test] fn openrouter_maps_effective_provider() { assert_eq!(native_mapping(&model("openrouter","anthropic/claude-sonnet-4")).unwrap().provider,SearchProvider::Anthropic); assert!(native_mapping(&model("openrouter","openrouter/x")).is_none()); }
    #[test] fn codex_model_is_not_discovered() { assert!(native_mapping(&model("openai","gpt-5-codex")).is_none()); assert!(native_mapping(&model("openai","gpt-5.5")).is_some()); }
    #[test] fn endpoints_preserve_query_and_drop_fragment() { assert_eq!(build_endpoint_url("https://example.test/v2/?a=1#fragment","responses",None),"https://example.test/v2/responses?a=1"); assert_eq!(build_endpoint_url("https://example.test/foo","messages",Some("/anthropic/v1/messages")),"https://example.test/anthropic/v1/messages"); }
    #[test] fn route_key_normalizes_terminal_dot_and_rejects_private_host() { let mut model=model("openai","gpt-5"); model.base_url="https://api.example.test./v1".into(); assert_eq!(native_route_key(&model).unwrap(),"openai|https://api.example.test/v1/responses"); model.base_url="https://localhost/v1".into(); assert!(native_route_key(&model).is_none()); }
}
