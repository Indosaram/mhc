use std::path::Path;
use serde_json::{Value,Map};
use super::{types::*,provider_endpoints::is_allowed_provider_base_url};
fn string(raw:&Value,key:&str)->Option<String> { raw.get(key).and_then(Value::as_str).filter(|s|!s.is_empty()).map(str::to_owned) }
fn number(raw:&Value,key:&str)->Option<f64> { raw.get(key).and_then(Value::as_f64).filter(|n|n.is_finite()) }
fn strings(raw:&Value,key:&str)->Option<Vec<String>> { raw.get(key)?.as_array()?.iter().map(|value|value.as_str().map(str::to_owned)).collect() }
fn provider(value:&str)->Option<SearchProvider> { Some(match value { "exa"=>SearchProvider::Exa,"tavily"=>SearchProvider::Tavily,"brave"=>SearchProvider::Brave,"duckduckgo-html"=>SearchProvider::DuckduckgoHtml,"deepseek"=>SearchProvider::Deepseek,"serper"=>SearchProvider::Serper,"google-cse"=>SearchProvider::GoogleCse,"z-ai"=>SearchProvider::Zai,"openai"=>SearchProvider::Openai,"codex"=>SearchProvider::Codex,"anthropic"=>SearchProvider::Anthropic,"perplexity"=>SearchProvider::Perplexity,"xai"=>SearchProvider::Xai,"kimi"=>SearchProvider::Kimi,_=>return None }) }
fn entry(raw:&Value)->Option<SearchProviderEntry> {
    raw.as_object()?;
    let kind=raw.get("provider").and_then(Value::as_str).and_then(provider).or_else(||raw.get("backend").and_then(Value::as_str).and_then(provider))?;
    let mut config=SearchProviderConfig::new(kind);
    config.id=string(raw,"id"); config.api_key=string(raw,"apiKey"); config.base_url=string(raw,"baseUrl"); config.search_engine_id=string(raw,"searchEngineId"); config.max_results=number(raw,"maxResults").filter(|n|*n!=0.); config.model=string(raw,"model");
    config.codex_mode=match string(raw,"codexMode").as_deref() { Some("cached")=>Some(CodexSearchMode::Cached),Some("live")=>Some(CodexSearchMode::Live),_=>None };
    config.search_context_size=match string(raw,"searchContextSize").as_deref() { Some("low")=>Some(SearchContextSize::Low),Some("medium")=>Some(SearchContextSize::Medium),Some("high")=>Some(SearchContextSize::High),_=>None };
    config.allowed_domains=strings(raw,"allowedDomains"); config.blocked_domains=strings(raw,"blockedDomains"); config.timeout_ms=number(raw,"timeoutMs");
    if let Some(location)=raw.get("userLocation").filter(|location|location.is_object()) { let mut object=Map::new(); for key in ["country","region","city","timezone"] { if let Some(value)=string(location,key) { object.insert(key.into(),Value::String(value)); } } if !object.is_empty() { config.user_location=Some(Value::Object(object)); } }
    Some(SearchProviderEntry{config,priority:number(raw,"priority"),weight:number(raw,"weight")})
}
pub fn config_from_object(raw:&Value)->Option<WebsearchConfig> {
    raw.as_object()?;
    let auto=raw.get("auto").and_then(Value::as_bool).unwrap_or(true);
    if let Some(providers)=raw.get("providers").and_then(Value::as_array) {
        if raw.get("provider").and_then(Value::as_str).and_then(provider).is_some() { return None; }
        let strategy=match string(raw,"strategy").as_deref() { Some("round-robin")=>RoutingStrategy::RoundRobin,Some("fill-first")=>RoutingStrategy::FillFirst,_=>RoutingStrategy::Priority };
        Some(WebsearchConfig{strategy,fallback:raw.get("fallback").and_then(Value::as_bool).unwrap_or(true),auto,providers:providers.iter().filter_map(entry).collect()})
    } else { Some(WebsearchConfig{strategy:RoutingStrategy::Priority,fallback:true,auto,providers:vec![entry(raw)?]}) }
}
pub fn validate_provider_config(entry:&SearchProviderEntry)->Result<(),(ConfigLoadFailureReason,String)> {
    let config=&entry.config;
    let invalid=|message:&str|Err((ConfigLoadFailureReason::InvalidConfig,message.into()));
    if config.allowed_domains.is_some() && config.blocked_domains.is_some() { return invalid("Provider config cannot specify both allowedDomains and blockedDomains."); }
    if entry.weight.is_some_and(|weight|weight<=0.) { return invalid("Provider weight must be greater than 0."); }
    if config.timeout_ms.is_some_and(|timeout|timeout<=0.) { return invalid("Provider timeoutMs must be greater than 0."); }
    if config.base_url.as_ref().is_some_and(|url|!url.is_empty() && !is_allowed_provider_base_url(url)) { return Err((ConfigLoadFailureReason::InvalidConfig,format!("Provider {} baseUrl must be a public HTTPS URL without credentials.",config.provider.as_str()))); }
    let missing=|message:String|Err((ConfigLoadFailureReason::MissingApiKey,message));
    if config.provider==SearchProvider::GoogleCse && config.search_engine_id.as_ref().is_none_or(String::is_empty) { return missing("Provider google-cse requires searchEngineId.".into()); }
    if config.api_key.as_ref().is_none_or(String::is_empty) { match config.provider {
        SearchProvider::Openai|SearchProvider::Codex=>return missing(format!("Provider {} requires apiKey for hosted Responses API search.",config.provider.as_str())),SearchProvider::DuckduckgoHtml=>{},_=>return missing(format!("Provider {} requires apiKey.",config.provider.as_str())),
    } }
    Ok(())
}
pub fn validate_websearch_config(config:&WebsearchConfig)->Result<(),(ConfigLoadFailureReason,String)> { if config.providers.is_empty() { return Err((ConfigLoadFailureReason::InvalidConfig,"Websearch config requires at least one provider.".into())); } for provider in &config.providers { validate_provider_config(provider)?; } Ok(()) }
pub async fn load_websearch_config(cwd:&Path,home:&Path)->Result<ConfigLoadResult,std::io::Error> {
    let paths=[cwd.join(".senpi/websearch.json"),cwd.join(".pi/websearch.json"),home.join("websearch.json"),home.join(".senpi/websearch.json"),home.join(".pi/websearch.json")];
    let mut seen=Vec::new();
    for path in paths { if seen.contains(&path) { continue; } seen.push(path.clone()); if tokio::fs::metadata(&path).await.is_err() { continue; }
        let bytes=tokio::fs::read(&path).await?; let text=String::from_utf8_lossy(&bytes); let source=path.to_string_lossy().into_owned();
        let raw:Value=match serde_json::from_str::<Value>(&text) { Ok(value) if value.is_object()=>value,_=>return Ok(ConfigLoadResult::Err{reason:ConfigLoadFailureReason::InvalidConfig,message:format!("Invalid JSON object in {source}"),source:Some(source)}) };
        let Some(config)=config_from_object(&raw) else { return Ok(ConfigLoadResult::Err{reason:ConfigLoadFailureReason::InvalidConfig,message:format!("Invalid provider config in {source}"),source:Some(source)}); };
        return Ok(match validate_websearch_config(&config) { Ok(())=>ConfigLoadResult::Ok{config,source},Err((reason,message))=>ConfigLoadResult::Err{reason,message,source:Some(source)} });
    }
    let mut provider=SearchProviderConfig::new(SearchProvider::DuckduckgoHtml); provider.id=Some("default".into()); provider.max_results=Some(10.);
    Ok(ConfigLoadResult::Ok{config:WebsearchConfig{strategy:RoutingStrategy::Priority,fallback:true,auto:true,providers:vec![SearchProviderEntry{config:provider,priority:None,weight:None}]},source:"default:duckduckgo-html".into()})
}
#[cfg(test)]
mod tests {
    use super::*; use serde_json::json;
    #[tokio::test] async fn upstream_project_config_path_priority() {
        for (paths,winner) in [(vec![".senpi/websearch.json"],".senpi/websearch.json"),(vec![".pi/websearch.json"],".pi/websearch.json"),(vec![".senpi/websearch.json",".pi/websearch.json"],".senpi/websearch.json")] {
            let cwd=tempfile::tempdir().unwrap(); let home=tempfile::tempdir().unwrap();
            std::fs::create_dir(home.path().join(".senpi")).unwrap();
            std::fs::write(home.path().join(".senpi/websearch.json"),r#"{"provider":"duckduckgo-html","id":"home"}"#).unwrap();
            for path in paths { let path=cwd.path().join(path); std::fs::create_dir_all(path.parent().unwrap()).unwrap(); std::fs::write(path,r#"{"provider":"duckduckgo-html","id":"project"}"#).unwrap(); }
            let ConfigLoadResult::Ok{config,source}=load_websearch_config(cwd.path(),home.path()).await.unwrap() else { panic!("expected project config"); };
            assert_eq!(source,cwd.path().join(winner).to_string_lossy()); assert_eq!(config.providers[0].config.id.as_deref(),Some("project"));
        }
    }
    #[test] fn backend_alias_and_false_auto_are_preserved() { let config=config_from_object(&json!({"backend":"duckduckgo-html","auto":false,"maxResults":0,"timeoutMs":0})).unwrap(); assert!(!config.auto); assert_eq!(config.providers[0].config.max_results,None); assert_eq!(config.providers[0].config.timeout_ms,Some(0.)); assert!(validate_websearch_config(&config).is_err()); }
    #[test] fn invalid_entries_are_filtered_before_validation() { let config=config_from_object(&json!({"providers":[null,{"provider":"invalid"},{"provider":"duckduckgo-html"}],"strategy":"round-robin"})).unwrap(); assert_eq!(config.providers.len(),1); assert_eq!(config.strategy,RoutingStrategy::RoundRobin); assert!(validate_websearch_config(&config).is_ok()); }
    #[test] fn mixed_single_and_list_is_rejected() { assert!(config_from_object(&json!({"provider":"exa","providers":[]})).is_none()); }
    #[test] fn empty_domain_lists_still_conflict() { let config=config_from_object(&json!({"provider":"duckduckgo-html","allowedDomains":[],"blockedDomains":[]})).unwrap(); assert_eq!(validate_websearch_config(&config).unwrap_err().0,ConfigLoadFailureReason::InvalidConfig); }
    #[test] fn hosted_search_requires_key() { let config=config_from_object(&json!({"provider":"openai"})).unwrap(); assert_eq!(validate_websearch_config(&config).unwrap_err().0,ConfigLoadFailureReason::MissingApiKey); }
    #[test] fn private_override_rejected_before_missing_key() { let config=config_from_object(&json!({"provider":"exa","baseUrl":"https://localhost"})).unwrap(); assert_eq!(validate_websearch_config(&config).unwrap_err().0,ConfigLoadFailureReason::InvalidConfig); }
    #[tokio::test] async fn config_read_replaces_invalid_utf8_like_node() {
        let dir=tempfile::tempdir().unwrap();
        let cwd=dir.path().join("project"); let home=dir.path().join("home");
        tokio::fs::create_dir_all(cwd.join(".senpi")).await.unwrap();
        let mut bytes=b"{\"provider\":\"duckduckgo-html\",\"auto\":false,\"id\":\"".to_vec();
        bytes.push(0xff); bytes.extend_from_slice(b"\"}");
        tokio::fs::write(cwd.join(".senpi/websearch.json"),bytes).await.unwrap();
        let result=load_websearch_config(&cwd,&home).await.unwrap();
        match result {
            ConfigLoadResult::Ok{config,..}=>assert_eq!(config.providers[0].config.id.as_deref(),Some("\u{fffd}")),
            _=>panic!("expected decoded configuration"),
        }
    }
    #[tokio::test] async fn invalid_project_config_does_not_fall_through_to_home() {
        let dir=tempfile::tempdir().unwrap();
        let cwd=dir.path().join("project"); let home=dir.path().join("home");
        tokio::fs::create_dir_all(cwd.join(".senpi")).await.unwrap();
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(cwd.join(".senpi/websearch.json"),b"\xff").await.unwrap();
        tokio::fs::write(home.join("websearch.json"),b"{\"provider\":\"duckduckgo-html\",\"auto\":false}").await.unwrap();
        let result=load_websearch_config(&cwd,&home).await.unwrap();
        match result {
            ConfigLoadResult::Err{reason,source,..}=>{
                assert_eq!(reason,ConfigLoadFailureReason::InvalidConfig);
                assert_eq!(source,Some(cwd.join(".senpi/websearch.json").to_string_lossy().into_owned()));
            },
            _=>panic!("expected invalid first configuration"),
        }
    }
}
