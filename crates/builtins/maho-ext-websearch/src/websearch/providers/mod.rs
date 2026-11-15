pub mod anthropic;
pub mod brave;
pub mod chatgpt_subscription;
pub mod deepseek;
pub mod duckduckgo_html;
pub mod ecosia;
pub mod exa;
pub mod exa_mcp;
pub mod firecrawl;
pub mod google;
pub mod google_cse;
pub mod google_html;
pub mod jina;
pub mod kagi;
pub mod keenable;
pub mod kimi;
pub mod mojeek;
pub mod ollama;
pub mod openai_responses;
pub mod openrouter;
pub mod parallel;
pub mod perplexity;
pub mod public;
pub mod response_stream;
pub mod searxng;
pub mod serpdive;
pub mod serper;
pub mod shared;
pub mod startpage;
pub mod synthetic;
pub mod tavily;
pub mod tinyfish;
pub mod xai;
pub mod z_ai;
use super::{
    provider_endpoints::SearchProvider,
    types::{SearchProviderConfig, SearchRequest},
};
use serde_json::Value;
use shared::{BuildContext, BuiltSearchRequest, SearchResultItem, resolve_domain_filters};
/// Why a provider could not turn a request into an HTTP call: a bad configured URL, or a provider
/// that has no endpoint of its own (`public` fans out instead of calling one).
#[derive(Debug)]
pub enum SearchBuildError {
    Url(url::ParseError),
    Message(String),
}
impl std::fmt::Display for SearchBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(error) => error.fmt(f),
            Self::Message(message) => message.fmt(f),
        }
    }
}
impl From<url::ParseError> for SearchBuildError {
    fn from(error: url::ParseError) -> Self {
        Self::Url(error)
    }
}
pub fn build_search_request(
    config: &SearchProviderConfig,
    request: &SearchRequest,
) -> Result<BuiltSearchRequest, SearchBuildError> {
    let (allowed, blocked) = resolve_domain_filters(
        config.allowed_domains.as_deref(),
        config.blocked_domains.as_deref(),
        request.allowed_domains.as_deref(),
        request.blocked_domains.as_deref(),
    );
    let ctx = BuildContext {
        query: &request.query,
        max_results: config.max_results.unwrap_or(request.max_results),
        api_key: config.api_key.as_deref(),
        base_url: config.base_url.as_deref(),
        allowed_domains: allowed.as_deref(),
        blocked_domains: blocked.as_deref(),
    };
    let model = config.model.as_deref();
    let size = config.search_context_size.map(|size| size.as_str());
    let mut built = match config.provider {
        SearchProvider::Exa => exa::build_request(&ctx),
        SearchProvider::Tavily => tavily::build_request(&ctx),
        SearchProvider::Brave => brave::build_request(&ctx)?,
        SearchProvider::DuckduckgoHtml => duckduckgo_html::build_request(&ctx)?,
        SearchProvider::Deepseek => deepseek::build_request(&ctx, model),
        SearchProvider::Serper => serper::build_request(&ctx),
        SearchProvider::GoogleCse => {
            google_cse::build_request(&ctx, config.search_engine_id.as_deref())?
        }
        SearchProvider::Google => google::build_request(&ctx, model),
        SearchProvider::Zai => z_ai::build_request(&ctx, model, size),
        SearchProvider::Openai | SearchProvider::Codex => openai_responses::build_request(
            &ctx,
            config.provider,
            model,
            config.codex_mode.map(|mode| mode.as_str()),
            size,
            config.user_location.as_ref(),
        ),
        SearchProvider::ChatgptSubscription => chatgpt_subscription::build_request(
            &ctx,
            model,
            config.codex_mode.map(|mode| mode.as_str()),
            size,
            config.user_location.as_ref(),
        ),
        SearchProvider::Anthropic => anthropic::build_request(&ctx, model),
        SearchProvider::Perplexity => perplexity::build_request(&ctx, model, size),
        SearchProvider::Xai => xai::build_request(&ctx, model),
        SearchProvider::Kimi => kimi::build_request(&ctx),
        SearchProvider::Kagi => kagi::build_request(&ctx),
        SearchProvider::Startpage => startpage::build_request(&ctx)?,
        SearchProvider::Mojeek => mojeek::build_request(&ctx)?,
        SearchProvider::Ecosia => ecosia::build_request(&ctx)?,
        SearchProvider::GoogleHtml => google_html::build_request(&ctx)?,
        SearchProvider::Searxng => searxng::build_request(&ctx)?,
        SearchProvider::Serpdive => serpdive::build_request(&ctx),
        SearchProvider::ExaMcp => exa_mcp::build_request(&ctx)?,
        SearchProvider::Keenable => keenable::build_request(&ctx),
        SearchProvider::Firecrawl => firecrawl::build_request(&ctx),
        SearchProvider::Jina => jina::build_request(&ctx),
        SearchProvider::Ollama => ollama::build_request(&ctx),
        SearchProvider::OpenRouter => openrouter::build_request(&ctx, model),
        SearchProvider::Parallel => parallel::build_request(&ctx),
        SearchProvider::Synthetic => synthetic::build_request(&ctx),
        SearchProvider::Tinyfish => tinyfish::build_request(&ctx),
        SearchProvider::Public => {
            return Err(SearchBuildError::Message(
                "Provider public has no endpoint of its own; it fans out to its engines.".into(),
            ));
        }
    };
    if config.provider == SearchProvider::ChatgptSubscription {
        built.headers = chatgpt_subscription::with_config_headers(
            config.headers.as_ref(),
            std::mem::take(&mut built.headers),
        );
    } else {
        shared::with_auth_headers(config.headers.as_ref(), &mut built.headers);
    }
    Ok(built)
}
/// senpi `parseBody`: providers whose reply is a streamed body fold it into a JSON payload first.
pub fn parse_search_body(provider: SearchProvider, body_text: &str) -> Option<Value> {
    match provider {
        SearchProvider::ChatgptSubscription => Some(
            super::providers::response_stream::collect_response_stream(body_text),
        ),
        _ => None,
    }
}
pub fn normalize_search_response(
    provider: SearchProvider,
    payload: &Value,
) -> Vec<SearchResultItem> {
    match provider {
        SearchProvider::Exa => exa::normalize_response(payload),
        SearchProvider::Tavily => tavily::normalize_response(payload),
        SearchProvider::Brave => brave::normalize_response(payload),
        SearchProvider::DuckduckgoHtml => duckduckgo_html::normalize_response(payload),
        SearchProvider::Deepseek => deepseek::normalize_response(payload),
        SearchProvider::Serper => serper::normalize_response(payload),
        SearchProvider::GoogleCse => google_cse::normalize_response(payload),
        SearchProvider::Google => google::normalize_response(payload),
        SearchProvider::Zai => z_ai::normalize_response(payload),
        SearchProvider::Openai | SearchProvider::Codex => {
            openai_responses::normalize_response(payload)
        }
        SearchProvider::ChatgptSubscription => chatgpt_subscription::normalize_response(payload),
        SearchProvider::Anthropic => anthropic::normalize_response(payload),
        SearchProvider::Perplexity => perplexity::normalize_response(payload),
        SearchProvider::Xai => xai::normalize_response(payload),
        SearchProvider::Kimi => kimi::normalize_response(payload),
        SearchProvider::Kagi => kagi::normalize_response(payload),
        SearchProvider::Startpage => {
            startpage::normalize_html(payload["html"].as_str().unwrap_or_default())
        }
        SearchProvider::Mojeek => {
            mojeek::normalize_html(payload["html"].as_str().unwrap_or_default())
        }
        SearchProvider::Ecosia => {
            ecosia::normalize_html(payload["html"].as_str().unwrap_or_default())
        }
        SearchProvider::GoogleHtml => {
            google_html::normalize_html(payload["html"].as_str().unwrap_or_default())
        }
        SearchProvider::Searxng => searxng::normalize_response(payload),
        SearchProvider::Serpdive => serpdive::normalize_response(payload),
        SearchProvider::ExaMcp => exa_mcp::normalize_response(payload),
        SearchProvider::Keenable => keenable::normalize_response(payload),
        SearchProvider::Firecrawl => firecrawl::normalize_response(payload),
        SearchProvider::Jina => jina::normalize_response(payload),
        SearchProvider::Ollama => ollama::normalize_response(payload),
        SearchProvider::OpenRouter => openrouter::normalize_response(payload),
        SearchProvider::Parallel => parallel::normalize_response(payload),
        SearchProvider::Synthetic => synthetic::normalize_response(payload),
        SearchProvider::Tinyfish => tinyfish::normalize_response(payload),
        SearchProvider::Public => Vec::new(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn config_limits_precede_request_and_domains_cannot_expand() {
        let mut config = SearchProviderConfig::new(SearchProvider::Exa);
        config.max_results = Some(2.);
        config.allowed_domains = Some(vec!["a".into()]);
        let request = SearchRequest {
            query: "q".into(),
            max_results: 10.,
            allowed_domains: Some(vec!["b".into()]),
            blocked_domains: None,
        };
        let built = build_search_request(&config, &request).unwrap();
        assert_eq!(built.body.as_ref().expect("request body")["numResults"], 2.);
        assert_eq!(
            built.body.as_ref().expect("request body")["includeDomains"],
            json!(["invalid.invalid"])
        );
    }
    #[test]
    fn all_providers_dispatch_without_network_and_ignore_primitive_payload() {
        for provider in [
            SearchProvider::Exa,
            SearchProvider::Tavily,
            SearchProvider::Brave,
            SearchProvider::DuckduckgoHtml,
            SearchProvider::Deepseek,
            SearchProvider::Serper,
            SearchProvider::GoogleCse,
            SearchProvider::Google,
            SearchProvider::Zai,
            SearchProvider::Openai,
            SearchProvider::Codex,
            SearchProvider::ChatgptSubscription,
            SearchProvider::Anthropic,
            SearchProvider::Perplexity,
            SearchProvider::Xai,
            SearchProvider::Kimi,
            SearchProvider::Kagi,
            SearchProvider::Startpage,
            SearchProvider::Mojeek,
            SearchProvider::Ecosia,
            SearchProvider::GoogleHtml,
            SearchProvider::ExaMcp,
            SearchProvider::Serpdive,
            SearchProvider::Keenable,
            SearchProvider::Firecrawl,
            SearchProvider::Jina,
            SearchProvider::Ollama,
            SearchProvider::OpenRouter,
            SearchProvider::Parallel,
            SearchProvider::Synthetic,
            SearchProvider::Tinyfish,
        ] {
            let config = SearchProviderConfig::new(provider);
            let request = SearchRequest {
                query: "q".into(),
                max_results: 1.,
                allowed_domains: None,
                blocked_domains: None,
            };
            assert!(
                !build_search_request(&config, &request)
                    .unwrap()
                    .url
                    .is_empty()
            );
            assert!(normalize_search_response(provider, &Value::Bool(true)).is_empty());
        }
    }

    #[test]
    fn keyed_and_self_hosted_providers_need_their_own_endpoints() {
        let request = SearchRequest {
            query: "q".into(),
            max_results: 1.,
            allowed_domains: None,
            blocked_domains: None,
        };
        let error =
            build_search_request(&SearchProviderConfig::new(SearchProvider::Public), &request)
                .expect_err("public fans out instead of calling an endpoint");
        assert_eq!(
            error.to_string(),
            "Provider public has no endpoint of its own; it fans out to its engines."
        );
        let error = build_search_request(
            &SearchProviderConfig::new(SearchProvider::Searxng),
            &request,
        )
        .expect_err("searxng is self-hosted and needs a base URL");
        assert!(error.to_string().contains("relative URL without a base"));
        let mut searxng = SearchProviderConfig::new(SearchProvider::Searxng);
        searxng.base_url = Some("https://searx.test".into());
        assert_eq!(
            build_search_request(&searxng, &request)
                .expect("searxng request")
                .url,
            "https://searx.test/search?q=q&format=json"
        );
    }
}
