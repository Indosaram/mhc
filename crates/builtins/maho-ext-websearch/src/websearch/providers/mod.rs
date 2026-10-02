pub mod shared;
pub mod exa;
pub mod tavily;
pub mod brave;
pub mod serper;
pub mod google_cse;
pub mod z_ai;
pub mod perplexity;
pub mod kimi;
pub mod anthropic;
pub mod deepseek;
pub mod duckduckgo_html;
pub mod openai_responses;
pub mod xai;
use serde_json::Value;
use super::{provider_endpoints::SearchProvider,types::{SearchProviderConfig,SearchRequest}};
use shared::{BuildContext,BuiltSearchRequest,SearchResultItem,resolve_domain_filters};
pub fn build_search_request(config:&SearchProviderConfig,request:&SearchRequest)->Result<BuiltSearchRequest,url::ParseError> {
    let (allowed,blocked)=resolve_domain_filters(config.allowed_domains.as_deref(),config.blocked_domains.as_deref(),request.allowed_domains.as_deref(),request.blocked_domains.as_deref());
    let ctx=BuildContext{query:&request.query,max_results:config.max_results.unwrap_or(request.max_results),api_key:config.api_key.as_deref(),base_url:config.base_url.as_deref(),allowed_domains:allowed.as_deref(),blocked_domains:blocked.as_deref()};
    let model=config.model.as_deref(); let size=config.search_context_size.map(|size|size.as_str());
    Ok(match config.provider {
        SearchProvider::Exa=>exa::build_request(&ctx),SearchProvider::Tavily=>tavily::build_request(&ctx),SearchProvider::Brave=>brave::build_request(&ctx)?,SearchProvider::DuckduckgoHtml=>duckduckgo_html::build_request(&ctx)?,SearchProvider::Deepseek=>deepseek::build_request(&ctx,model),SearchProvider::Serper=>serper::build_request(&ctx),SearchProvider::GoogleCse=>google_cse::build_request(&ctx,config.search_engine_id.as_deref())?,SearchProvider::Zai=>z_ai::build_request(&ctx,model,size),SearchProvider::Openai|SearchProvider::Codex=>openai_responses::build_request(&ctx,config.provider,model,config.codex_mode.map(|mode|mode.as_str()),size,config.user_location.as_ref()),SearchProvider::Anthropic=>anthropic::build_request(&ctx,model),SearchProvider::Perplexity=>perplexity::build_request(&ctx,model,size),SearchProvider::Xai=>xai::build_request(&ctx,model),SearchProvider::Kimi=>kimi::build_request(&ctx),
    })
}
pub fn normalize_search_response(provider:SearchProvider,payload:&Value)->Vec<SearchResultItem> {
    match provider {
        SearchProvider::Exa=>exa::normalize_response(payload),SearchProvider::Tavily=>tavily::normalize_response(payload),SearchProvider::Brave=>brave::normalize_response(payload),SearchProvider::DuckduckgoHtml=>duckduckgo_html::normalize_response(payload),SearchProvider::Deepseek=>deepseek::normalize_response(payload),SearchProvider::Serper=>serper::normalize_response(payload),SearchProvider::GoogleCse=>google_cse::normalize_response(payload),SearchProvider::Zai=>z_ai::normalize_response(payload),SearchProvider::Openai|SearchProvider::Codex=>openai_responses::normalize_response(payload),SearchProvider::Anthropic=>anthropic::normalize_response(payload),SearchProvider::Perplexity=>perplexity::normalize_response(payload),SearchProvider::Xai=>xai::normalize_response(payload),SearchProvider::Kimi=>kimi::normalize_response(payload),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test] fn config_limits_precede_request_and_domains_cannot_expand() { let mut config=SearchProviderConfig::new(SearchProvider::Exa); config.max_results=Some(2.); config.allowed_domains=Some(vec!["a".into()]); let request=SearchRequest{query:"q".into(),max_results:10.,allowed_domains:Some(vec!["b".into()]),blocked_domains:None}; let built=build_search_request(&config,&request).unwrap(); assert_eq!(built.body["numResults"],2.); assert_eq!(built.body["includeDomains"],json!(["invalid.invalid"])); }
    #[test] fn all_providers_dispatch_without_network_and_ignore_primitive_payload() { for provider in [SearchProvider::Exa,SearchProvider::Tavily,SearchProvider::Brave,SearchProvider::DuckduckgoHtml,SearchProvider::Deepseek,SearchProvider::Serper,SearchProvider::GoogleCse,SearchProvider::Zai,SearchProvider::Openai,SearchProvider::Codex,SearchProvider::Anthropic,SearchProvider::Perplexity,SearchProvider::Xai,SearchProvider::Kimi] { let config=SearchProviderConfig::new(provider); let request=SearchRequest{query:"q".into(),max_results:1.,allowed_domains:None,blocked_domains:None}; assert!(!build_search_request(&config,&request).unwrap().url.is_empty()); assert!(normalize_search_response(provider,&Value::Bool(true)).is_empty()); } }
}
