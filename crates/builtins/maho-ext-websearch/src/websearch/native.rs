use super::provider_endpoints::{SearchProvider,is_allowed_provider_base_url};
pub fn discovered_native_entry_id(provider:SearchProvider,route_key:&str)->String {
    use sha2::{Digest,Sha256};
    let digest=Sha256::digest(route_key.as_bytes());
    let fingerprint=digest[..8].iter().map(|byte|format!("{byte:02x}")).collect::<String>();
    format!("native-{}-{fingerprint}",provider.as_str())
}
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct NativeModelInfo { pub provider:String,pub id:String,pub base_url:String,pub api:Option<String> }
pub fn model_info(model:&maho_ext_api::Model)->NativeModelInfo {NativeModelInfo{provider:model.provider.clone(),id:model.id.clone(),base_url:model.base_url.clone(),api:Some(model.api.clone())}}
pub async fn config_with_native_routes(mut config:super::types::WebsearchConfig,model:Option<&maho_ext_api::Model>,registry:Option<&std::sync::Arc<dyn maho_ext_api::ModelRegistry>>,signal:Option<&maho_tools::definition::AbortSignal>)->Result<super::types::WebsearchConfig,maho_tools::definition::ToolError> {
    if !config.auto{return Ok(config);}
    if let Some(signal)=signal{signal.check()?;}
    let Some(registry)=registry else{return Ok(config)};
    let mut models=registry.get_available();if let Some(model)=model{models.insert(0,model.clone());}
    let available=models.iter().map(model_info).collect::<Vec<_>>();let active=model.map(model_info);
    let auth=|info:NativeModelInfo|{let registry=registry.clone();let model=models.iter().find(|model|model_info(model)==info).cloned();async move {match model {Some(model)=>registry.get_api_key_and_headers(&model).await.ok().and_then(|auth|auth.auth.api_key),None=>None}}};
    let mut entries=build_native_entries(active.as_ref(),Some(&available),Some(&auth),signal).await?;
    if !entries.is_empty(){entries.extend(config.providers);config.providers=entries;}
    Ok(config)
}
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
    macro_rules! upstream_matrix {
        ($name:ident,$provider:literal,$id:literal,$base:literal,$expected:expr) => {
            #[tokio::test] async fn $name() {
                let model=NativeModelInfo{provider:$provider.into(),id:$id.into(),base_url:$base.into(),api:None};
                let auth=|_:NativeModelInfo|async {Some("fixture-key".into())};
                let entry=build_native_entry(Some(&model),Some(&auth),Some("native"),None).await.unwrap();
                let expected:Option<(SearchProvider,&str)>=$expected;
                match expected {
                    None=>assert!(entry.is_none()),
                    Some((provider,url))=>{let entry=entry.unwrap();assert_eq!(entry.config.provider,provider);assert_eq!(entry.config.base_url.as_deref(),Some(url));assert_eq!(entry.config.model.as_deref(),Some($id));assert_eq!(entry.config.api_key.as_deref(),Some("fixture-key"));assert_eq!(entry.priority,Some(-1.));assert_eq!(entry.config.id.as_deref(),Some("native"));}
                }
            }
        };
    }
    upstream_matrix!(matrix_gpt_sol,"openai","gpt-5.6-sol","https://gateway.example.com/v1",Some((SearchProvider::Openai,"https://gateway.example.com/v1/responses")));
    upstream_matrix!(matrix_gpt_terra,"openai","gpt-5.6-terra","https://gateway.example.com/v1",Some((SearchProvider::Openai,"https://gateway.example.com/v1/responses")));
    upstream_matrix!(matrix_gpt_55,"openai","gpt-5.5","https://gateway.example.com/v1",Some((SearchProvider::Openai,"https://gateway.example.com/v1/responses")));
    upstream_matrix!(matrix_gpt_fast,"openai","gpt-5.5-fast","https://gateway.example.com/v1",Some((SearchProvider::Openai,"https://gateway.example.com/v1/responses")));
    upstream_matrix!(matrix_gpt_54,"openai","gpt-5.4","https://gateway.example.com/v1",Some((SearchProvider::Openai,"https://gateway.example.com/v1/responses")));
    upstream_matrix!(matrix_gpt_pro,"openai","gpt-5-pro","https://gateway.example.com/v1",Some((SearchProvider::Openai,"https://gateway.example.com/v1/responses")));
    upstream_matrix!(matrix_gpt_5,"openai","gpt-5","https://gateway.example.com/v1",Some((SearchProvider::Openai,"https://gateway.example.com/v1/responses")));
    upstream_matrix!(matrix_gpt_41,"openai","gpt-4.1-mini","https://gateway.example.com/v1",Some((SearchProvider::Openai,"https://gateway.example.com/v1/responses")));
    upstream_matrix!(matrix_gpt_4o,"openai","gpt-4o-mini-2026-01-01","https://gateway.example.com/v1",Some((SearchProvider::Openai,"https://gateway.example.com/v1/responses")));
    upstream_matrix!(matrix_codex,"openai","gpt-5.3-codex","https://gateway.example.com/v1",None);
    upstream_matrix!(matrix_codex_spark,"openai","gpt-5.3-codex-spark","https://gateway.example.com/v1",None);
    upstream_matrix!(matrix_gpt_turbo,"openai","gpt-4-turbo","https://gateway.example.com/v1",None);
    upstream_matrix!(matrix_o3,"openai","o3","https://gateway.example.com/v1",None);
    upstream_matrix!(matrix_opus5,"anthropic","claude-opus-5","https://gateway.example.com/v1",Some((SearchProvider::Anthropic,"https://gateway.example.com/v1/messages")));
    upstream_matrix!(matrix_sonnet5,"anthropic","claude-sonnet-5","https://gateway.example.com/v1",Some((SearchProvider::Anthropic,"https://gateway.example.com/v1/messages")));
    upstream_matrix!(matrix_fable5,"anthropic","claude-fable-5","https://gateway.example.com/v1",Some((SearchProvider::Anthropic,"https://gateway.example.com/v1/messages")));
    upstream_matrix!(matrix_haiku,"anthropic","claude-haiku-4-5","https://gateway.example.com/v1",Some((SearchProvider::Anthropic,"https://gateway.example.com/v1/messages")));
    upstream_matrix!(matrix_opus48,"anthropic","claude-opus-4-8","https://gateway.example.com/v1",Some((SearchProvider::Anthropic,"https://gateway.example.com/v1/messages")));
    upstream_matrix!(matrix_sonnet_dated,"anthropic","claude-sonnet-4-5-20250929","https://gateway.example.com/v1",Some((SearchProvider::Anthropic,"https://gateway.example.com/v1/messages")));
    upstream_matrix!(matrix_not_claude,"anthropic","not-a-claude-model","https://gateway.example.com/v1",None);
    upstream_matrix!(matrix_grok,"xai","grok-4.3","https://gateway.example.com/v1",Some((SearchProvider::Xai,"https://gateway.example.com/v1/responses")));
    upstream_matrix!(matrix_openrouter,"openrouter","anthropic/claude-opus-5","https://openrouter.example.com/v1",Some((SearchProvider::Anthropic,"https://openrouter.example.com/v1/messages")));
    upstream_matrix!(matrix_deepseek_flash,"deepseek","deepseek-v4-flash","https://api.deepseek.com",Some((SearchProvider::Deepseek,"https://api.deepseek.com/anthropic/v1/messages")));
    upstream_matrix!(matrix_deepseek_pro,"deepseek","deepseek-v4-pro","https://api.deepseek.com",Some((SearchProvider::Deepseek,"https://api.deepseek.com/anthropic/v1/messages")));
    upstream_matrix!(matrix_deepseek_v3,"deepseek","deepseek-v3","https://api.deepseek.com",None);
    upstream_matrix!(matrix_deepseek_chat,"deepseek","deepseek-chat","https://api.deepseek.com",None);
    fn aliases(provider:&str,prefix:&str,count:usize)->Vec<NativeModelInfo> {(0..count).map(|index|model(provider,&if index==0 && provider=="anthropic" {prefix.into()} else {format!("{prefix}-{index}")})).collect()}
    #[tokio::test] async fn upstream_fourteen_aliases_yield_two_stable_opaque_routes() {
        let models=aliases("anthropic","claude-opus-4",8).into_iter().chain(aliases("z-ai","glm-4.6",6)).collect::<Vec<_>>();
        let calls=std::sync::Mutex::new(Vec::new());let auth=|model:NativeModelInfo| {calls.lock().unwrap().push(model.id);async {Some("fixture-key".into())}};
        let first=build_native_entries(None,Some(&models),Some(&auth),None).await.unwrap();
        let second=build_native_entries(None,Some(&models),Some(&auth),None).await.unwrap();
        assert_eq!(first.len(),2);assert_eq!(first,second);assert_ne!(first[0].config.id,first[1].config.id);
        for entry in &first {let id=entry.config.id.as_ref().unwrap();assert!(id.starts_with("native-"));for value in ["api.example","claude","glm","fixture-key"] {assert!(!id.contains(value));}}
        assert_eq!(*calls.lock().unwrap(),["claude-opus-4","glm-4.6-0","claude-opus-4","glm-4.6-0"]);
    }
    #[tokio::test] async fn upstream_active_auth_failure_does_not_retry_alias() {
        let active=model("openai","gpt-5.5");let calls=std::sync::Mutex::new(Vec::new());let auth=|model:NativeModelInfo| {calls.lock().unwrap().push(model.id);async {None}};
        assert!(build_native_entries(Some(&active),Some(&[model("openai","gpt-4.1")]),Some(&auth),None).await.unwrap().is_empty());assert_eq!(*calls.lock().unwrap(),["gpt-5.5"]);
    }
    #[tokio::test] async fn upstream_failed_alias_route_resolves_auth_once() {
        let calls=std::sync::Mutex::new(Vec::new());let auth=|model:NativeModelInfo| {calls.lock().unwrap().push(model.id);async {None}};
        assert!(build_native_entries(None,Some(&aliases("anthropic","claude-opus-4",8)),Some(&auth),None).await.unwrap().is_empty());assert_eq!(*calls.lock().unwrap(),["claude-opus-4"]);
    }
    #[tokio::test] async fn upstream_dotted_private_routes_rejected_before_auth() {
        let mut first=model("openai","gpt-5.5");first.base_url="https://localhost./v1".into();let mut second=model("openai","gpt-4.1");second.base_url="https://127.1../v1".into();
        let auth=|_:NativeModelInfo|async {panic!("private routes cannot resolve auth")};
        assert!(build_native_entries(None,Some(&[first,second]),Some(&auth),None).await.unwrap().is_empty());
    }
    #[tokio::test] async fn upstream_dotted_public_aliases_preserve_first_endpoint() {
        let mut first=model("openai","gpt-5.5");first.base_url="https://api.example.test./v1".into();let second=model("openai","gpt-4.1");
        let calls=std::sync::Mutex::new(Vec::new());let auth=|model:NativeModelInfo| {calls.lock().unwrap().push(model.id);async {Some("fixture-key".into())}};
        let entries=build_native_entries(None,Some(&[first,second]),Some(&auth),None).await.unwrap();assert_eq!(entries.len(),1);assert_eq!(entries[0].config.base_url.as_deref(),Some("https://api.example.test./v1/responses"));assert_eq!(*calls.lock().unwrap(),["gpt-5.5"]);
    }
    #[tokio::test] async fn upstream_distinct_endpoints_preserve_both_candidates() {
        let mut first=model("openai","gpt-4.1");first.base_url="https://a.example.test/v1".into();let mut second=model("openai","gpt-5.5");second.base_url="https://b.example.test/v1".into();
        let calls=std::sync::Mutex::new(Vec::new());let auth=|model:NativeModelInfo| {calls.lock().unwrap().push(model.id);async {Some("fixture-key".into())}};
        let entries=build_native_entries(None,Some(&[first,second]),Some(&auth),None).await.unwrap();assert_eq!(entries.len(),2);assert_ne!(entries[0].config.id,entries[1].config.id);
        assert_eq!(entries.iter().map(|entry|entry.config.base_url.as_deref().unwrap()).collect::<Vec<_>>(),["https://a.example.test/v1/responses","https://b.example.test/v1/responses"]);assert_eq!(*calls.lock().unwrap(),["gpt-4.1","gpt-5.5"]);
    }
    #[tokio::test] async fn upstream_query_preserved_fragments_deduplicated() {
        let mut first=model("openai","gpt-5.5");first.base_url="https://api.example.test/v1?token=fixture#first".into();let mut second=model("openai","gpt-4.1");second.base_url="https://api.example.test/v1?token=fixture#second".into();
        let calls=std::sync::Mutex::new(Vec::new());let auth=|model:NativeModelInfo| {calls.lock().unwrap().push(model.id);async {Some("fixture-key".into())}};
        let entries=build_native_entries(None,Some(&[first,second]),Some(&auth),None).await.unwrap();assert_eq!(entries.len(),1);assert_eq!(entries[0].config.base_url.as_deref(),Some("https://api.example.test/v1/responses?token=fixture"));assert!(!entries[0].config.id.as_ref().unwrap().contains("fixture"));assert_eq!(*calls.lock().unwrap(),["gpt-5.5"]);
    }
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
