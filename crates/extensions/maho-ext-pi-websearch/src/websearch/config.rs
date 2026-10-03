use std::path::Path;
use serde_json::{Map, Value};
use super::{types::*, provider_endpoints::is_allowed_provider_base_url};

fn optional_string(value: Option<&Value>) -> Option<String> { value.and_then(Value::as_str).filter(|value| !value.is_empty()).map(str::to_owned) }
fn optional_number(value: Option<&Value>) -> Option<f64> { value.and_then(Value::as_f64).filter(|number| number.is_finite()) }
fn optional_enum<T: serde::de::DeserializeOwned>(value: Option<&Value>) -> Option<T> { value.and_then(|value| serde_json::from_value(value.clone()).ok()) }
fn string_array(value: Option<&Value>) -> Option<Vec<String>> { value.and_then(Value::as_array).and_then(|values| values.iter().map(|value| value.as_str().map(str::to_owned)).collect()) }
fn provider_entry(raw: &Map<String, Value>) -> Option<SearchProviderEntry> {
    let provider=optional_enum(raw.get("provider")).or_else(|| optional_enum(raw.get("backend")))?;
    let user_location=raw.get("userLocation").and_then(Value::as_object).and_then(|location| {
        let location=SearchUserLocation{country:optional_string(location.get("country")),region:optional_string(location.get("region")),city:optional_string(location.get("city")),timezone:optional_string(location.get("timezone"))};
        (location.country.is_some()||location.region.is_some()||location.city.is_some()||location.timezone.is_some()).then_some(location)
    });
    Some(SearchProviderEntry{config:SearchProviderConfig{
        provider,id:optional_string(raw.get("id")),api_key:optional_string(raw.get("apiKey")),base_url:optional_string(raw.get("baseUrl")),search_engine_id:optional_string(raw.get("searchEngineId")),
        max_results:optional_number(raw.get("maxResults")).filter(|number| *number!=0.0),model:optional_string(raw.get("model")),codex_mode:optional_enum(raw.get("codexMode")),search_context_size:optional_enum(raw.get("searchContextSize")),
        allowed_domains:string_array(raw.get("allowedDomains")),blocked_domains:string_array(raw.get("blockedDomains")),user_location,timeout_ms:optional_number(raw.get("timeoutMs")),
    },priority:optional_number(raw.get("priority")),weight:optional_number(raw.get("weight"))})
}
pub fn config_from_object(raw: &Map<String, Value>) -> Option<WebsearchConfig> {
    let auto=raw.get("auto").and_then(Value::as_bool).unwrap_or(true);
    if let Some(providers)=raw.get("providers").and_then(Value::as_array) {
        if optional_enum::<SearchProvider>(raw.get("provider")).is_some(){return None;}
        Some(WebsearchConfig{strategy:optional_enum(raw.get("strategy")).unwrap_or(RoutingStrategy::Priority),fallback:raw.get("fallback").and_then(Value::as_bool).unwrap_or(true),auto,providers:providers.iter().filter_map(|value|value.as_object().and_then(provider_entry)).collect()})
    }else{Some(WebsearchConfig{strategy:RoutingStrategy::Priority,fallback:true,auto,providers:vec![provider_entry(raw)?]})}
}
pub fn validate_provider_config(config: SearchProviderEntry) -> ProviderValidationResult {
    let provider=&config.config;
    let failure=|reason,message|ProviderValidationResult::Failure{reason,message};
    if provider.allowed_domains.is_some()&&provider.blocked_domains.is_some(){return failure(ProviderValidationFailureReason::InvalidConfig,"Provider config cannot specify both allowedDomains and blockedDomains.".into());}
    if config.weight.is_some_and(|weight|weight<=0.0){return failure(ProviderValidationFailureReason::InvalidConfig,"Provider weight must be greater than 0.".into());}
    if provider.timeout_ms.is_some_and(|timeout|timeout<=0.0){return failure(ProviderValidationFailureReason::InvalidConfig,"Provider timeoutMs must be greater than 0.".into());}
    let name=match provider.provider{SearchProvider::Exa=>"exa",SearchProvider::Tavily=>"tavily",SearchProvider::Brave=>"brave",SearchProvider::DuckduckgoHtml=>"duckduckgo-html",SearchProvider::Serper=>"serper",SearchProvider::GoogleCse=>"google-cse",SearchProvider::Zai=>"z-ai",SearchProvider::Openai=>"openai",SearchProvider::Codex=>"codex",SearchProvider::Anthropic=>"anthropic",SearchProvider::Perplexity=>"perplexity",SearchProvider::Xai=>"xai",SearchProvider::Kimi=>"kimi"};
    if provider.base_url.as_deref().is_some_and(|url|!url.is_empty()&&!is_allowed_provider_base_url(url)){return failure(ProviderValidationFailureReason::InvalidConfig,format!("Provider {name} baseUrl must be a public HTTPS URL without credentials."));}
    if provider.provider==SearchProvider::GoogleCse&&provider.search_engine_id.as_deref().is_none_or(str::is_empty){return failure(ProviderValidationFailureReason::MissingApiKey,"Provider google-cse requires searchEngineId.".into());}
    if provider.api_key.as_deref().is_none_or(str::is_empty){
        match provider.provider{
            SearchProvider::DuckduckgoHtml=>{},
            SearchProvider::Codex|SearchProvider::Openai=>return failure(ProviderValidationFailureReason::MissingApiKey,format!("Provider {name} requires apiKey for hosted Responses API search.")),
            SearchProvider::Exa|SearchProvider::Tavily|SearchProvider::Brave|SearchProvider::Serper|SearchProvider::GoogleCse|SearchProvider::Zai|SearchProvider::Anthropic|SearchProvider::Perplexity|SearchProvider::Xai|SearchProvider::Kimi=>return failure(ProviderValidationFailureReason::MissingApiKey,format!("Provider {name} requires apiKey.")),
        }
    }
    ProviderValidationResult::Success{config:Box::new(config)}
}
pub fn validate_websearch_config(config: &WebsearchConfig) -> Result<(),(ConfigLoadFailureReason,String)> {
    if config.providers.is_empty(){return Err((ConfigLoadFailureReason::InvalidConfig,"Websearch config requires at least one provider.".into()));}
    for provider in &config.providers{if let ProviderValidationResult::Failure{reason,message}=validate_provider_config(provider.clone()){return Err((match reason{ProviderValidationFailureReason::InvalidConfig=>ConfigLoadFailureReason::InvalidConfig,ProviderValidationFailureReason::MissingApiKey=>ConfigLoadFailureReason::MissingApiKey},message));}}
    Ok(())
}
pub fn validate_websearch_config_value(value:&Value)->Result<WebsearchConfig,(ConfigLoadFailureReason,String)>{
    let raw=value.as_object().ok_or_else(||(ConfigLoadFailureReason::InvalidConfig,"Websearch config must be an object.".into()))?;
    if optional_enum::<RoutingStrategy>(raw.get("strategy")).is_none(){return Err((ConfigLoadFailureReason::InvalidConfig,format!("Unsupported routing strategy: {}",raw.get("strategy").and_then(Value::as_str).unwrap_or("undefined"))));}
    if raw.get("auto").and_then(Value::as_bool).is_none(){return Err((ConfigLoadFailureReason::InvalidConfig,"Websearch config auto must be a boolean.".into()));}
    let mut direct_providers=Vec::new();
    if let Some(providers)=raw.get("providers").and_then(Value::as_array){
        for provider in providers{
            let entry=provider.as_object().ok_or_else(||(ConfigLoadFailureReason::InvalidConfig,"Invalid provider config.".into()))?;
            if optional_enum::<SearchProvider>(entry.get("provider")).is_none(){return Err((ConfigLoadFailureReason::InvalidConfig,format!("Unsupported provider: {}",entry.get("provider").and_then(Value::as_str).unwrap_or("undefined"))));}
            let mut parsed=provider_entry(entry).ok_or_else(||(ConfigLoadFailureReason::InvalidConfig,"Invalid provider config.".into()))?;
            parsed.config.max_results=optional_number(entry.get("maxResults"));
            for (key,field) in [("id",&mut parsed.config.id),("apiKey",&mut parsed.config.api_key),("baseUrl",&mut parsed.config.base_url),("searchEngineId",&mut parsed.config.search_engine_id),("model",&mut parsed.config.model)]{
                *field=entry.get(key).and_then(Value::as_str).map(str::to_owned);
            }
            parsed.config.user_location=entry.get("userLocation").and_then(Value::as_object).map(|location|SearchUserLocation{
                country:location.get("country").and_then(Value::as_str).map(str::to_owned),
                region:location.get("region").and_then(Value::as_str).map(str::to_owned),
                city:location.get("city").and_then(Value::as_str).map(str::to_owned),
                timezone:location.get("timezone").and_then(Value::as_str).map(str::to_owned),
            });
            // Direct validation compares explicit null to zero; file loading omits it.
            if entry.get("weight").is_some_and(Value::is_null){parsed.weight=Some(0.0);}
            if entry.get("timeoutMs").is_some_and(Value::is_null){parsed.config.timeout_ms=Some(0.0);}
            if let ProviderValidationResult::Failure{reason,message}=validate_provider_config(parsed.clone()){return Err((match reason{ProviderValidationFailureReason::InvalidConfig=>ConfigLoadFailureReason::InvalidConfig,ProviderValidationFailureReason::MissingApiKey=>ConfigLoadFailureReason::MissingApiKey},message));}
            direct_providers.push(parsed);
        }
    }
    if !raw.get("providers").is_some_and(Value::is_array){return Err((ConfigLoadFailureReason::InvalidConfig,"Invalid provider config.".into()));}
    let config=WebsearchConfig{strategy:optional_enum(raw.get("strategy")).unwrap_or(RoutingStrategy::Priority),fallback:raw.get("fallback").and_then(Value::as_bool).unwrap_or(true),auto:raw.get("auto").and_then(Value::as_bool).unwrap_or(true),providers:direct_providers};
    validate_websearch_config(&config)?;Ok(config)
}
pub fn load_websearch_config(cwd: &Path,home: &Path) -> Result<ConfigLoadResult,std::io::Error> {
    for path in [cwd.join(".pi/websearch.json"),home.join("websearch.json"),home.join(".pi/websearch.json")] {
        if !path.try_exists().unwrap_or(false){continue;}
        let source=path.to_string_lossy().into_owned();
        let bytes=std::fs::read(&path)?;
        let content=String::from_utf8_lossy(&bytes);
        let raw=serde_json::from_str::<Value>(&content).ok();
        let Some(raw)=raw.as_ref().and_then(Value::as_object)else{return Ok(ConfigLoadResult::Failure{reason:ConfigLoadFailureReason::InvalidConfig,message:format!("Invalid JSON object in {source}"),source:Some(source)});};
        let Some(config)=config_from_object(raw)else{return Ok(ConfigLoadResult::Failure{reason:ConfigLoadFailureReason::InvalidConfig,message:format!("Invalid provider config in {source}"),source:Some(source)});};
        if let Err((reason,message))=validate_websearch_config(&config){return Ok(ConfigLoadResult::Failure{reason,message,source:Some(source)});}
        return Ok(ConfigLoadResult::Success{config,source});
    }
    let default=serde_json::json!({"providers":[{"id":"default","provider":"duckduckgo-html","maxResults":10}]});
    let config=config_from_object(default.as_object().unwrap_or_else(||unreachable!("literal object"))).unwrap_or_else(||unreachable!("literal default provider"));
    Ok(ConfigLoadResult::Success{config,source:"default:duckduckgo-html".into()})
}
