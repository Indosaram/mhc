use serde_json::Value;
use super::types::*;
use self::{shared::*,anthropic::AnthropicProvider,brave::BraveProvider,duckduckgo_html::DuckDuckGoHtmlProvider,exa::ExaProvider,google_cse::GoogleCseProvider,kimi::KimiProvider,openai_responses::OpenAiResponsesProvider,perplexity::PerplexityProvider,serper::SerperProvider,tavily::TavilyProvider,xai::XaiProvider,z_ai::ZAiProvider};
pub mod anthropic;
pub mod brave;
pub mod duckduckgo_html;
pub mod exa;
pub mod google_cse;
pub mod kimi;
pub mod openai_responses;
pub mod perplexity;
pub mod serper;
pub mod shared;
pub mod tavily;
pub mod xai;
pub mod z_ai;
pub fn build_search_request(config:&SearchProviderConfig,request:&SearchRequest)->Result<BuiltSearchRequest,url::ParseError>{
    let filters=resolve_domain_filters(config,request);
    let context=BuildContext{config,request,max_results:config.max_results.unwrap_or(request.max_results),allowed_domains:filters.allowed_domains,blocked_domains:filters.blocked_domains};
    Ok(match config.provider{
        SearchProvider::Exa=>ExaProvider.build_request(&context),
        SearchProvider::Tavily=>TavilyProvider.build_request(&context),
        SearchProvider::Brave=>BraveProvider.build_request(&context)?,
        SearchProvider::DuckduckgoHtml=>DuckDuckGoHtmlProvider.build_request(&context)?,
        SearchProvider::Serper=>SerperProvider.build_request(&context),
        SearchProvider::GoogleCse=>GoogleCseProvider.build_request(&context)?,
        SearchProvider::Zai=>ZAiProvider.build_request(&context),
        SearchProvider::Openai|SearchProvider::Codex=>OpenAiResponsesProvider.build_request(&context),
        SearchProvider::Anthropic=>AnthropicProvider.build_request(&context),
        SearchProvider::Perplexity=>PerplexityProvider.build_request(&context),
        SearchProvider::Xai=>XaiProvider.build_request(&context),
        SearchProvider::Kimi=>KimiProvider.build_request(&context),
    })
}
pub fn normalize_search_response(provider:SearchProvider,payload:&Value)->Vec<SearchResultItem>{
    let data=parse_object_payload(payload);
    match provider{
        SearchProvider::Exa=>ExaProvider.normalize_response(&data),
        SearchProvider::Tavily=>TavilyProvider.normalize_response(&data),
        SearchProvider::Brave=>BraveProvider.normalize_response(&data),
        SearchProvider::DuckduckgoHtml=>DuckDuckGoHtmlProvider.normalize_response(&data),
        SearchProvider::Serper=>SerperProvider.normalize_response(&data),
        SearchProvider::GoogleCse=>GoogleCseProvider.normalize_response(&data),
        SearchProvider::Zai=>ZAiProvider.normalize_response(&data),
        SearchProvider::Openai|SearchProvider::Codex=>OpenAiResponsesProvider.normalize_response(&data),
        SearchProvider::Anthropic=>AnthropicProvider.normalize_response(&data),
        SearchProvider::Perplexity=>PerplexityProvider.normalize_response(&data),
        SearchProvider::Xai=>XaiProvider.normalize_response(&data),
        SearchProvider::Kimi=>KimiProvider.normalize_response(&data),
    }
}
