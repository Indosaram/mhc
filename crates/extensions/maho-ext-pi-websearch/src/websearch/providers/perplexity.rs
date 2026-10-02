use std::collections::BTreeMap;
use serde_json::{Map, Value, json};
use super::{shared::*, super::{types::*, provider_endpoints::provider_url}};

fn normalize_result_list(items: &[Value]) -> Vec<SearchResultItem> {
    collect(items.iter().map(|raw| {
        let item = get_object(Some(raw))?;
        let mut found = result(get_string(item.get("title")), get_string(item.get("url")), get_string(item.get("snippet")), None, None)?;
        found.published_at = get_string(item.get("date")).or_else(|| get_string(item.get("last_updated"))).filter(|value| !value.is_empty()).map(str::to_owned);
        Some(found)
    }).collect(), 50)
}
pub struct PerplexityProvider;
impl ProviderModule for PerplexityProvider {
    fn build_request(&self, context: &BuildContext<'_>) -> BuiltSearchRequest {
        let mut body = if let Some(model) = context.config.model.as_deref().filter(|model| !model.is_empty()) {
            let mut body = Map::from_iter([("model".into(), json!(model)), ("messages".into(), json!([{ "role": "user", "content": context.request.query }]))]);
            if let Some(size) = context.config.search_context_size { body.insert("web_search_options".into(), json!({"search_context_size":size})); }
            body
        } else { Map::from_iter([("query".into(), json!(context.request.query)), ("max_results".into(), json!(clamp(context.max_results, 1.0, 20.0)))]) };
        if let Some(domains) = &context.allowed_domains { body.insert("search_domain_filter".into(), json!(domains)); }
        else if let Some(domains) = &context.blocked_domains { body.insert("search_domain_filter".into(), json!(domains.iter().map(|domain| format!("-{domain}")).collect::<Vec<_>>())); }
        BuiltSearchRequest {
            url: provider_url(context.config.provider, context.config.base_url.as_deref()).into(),
            init: SearchRequestInit { method: HttpMethod::Post, headers: content_headers(Some(&BTreeMap::from([("Authorization".into(), format!("Bearer {}", context.config.api_key.as_deref().unwrap_or("")))]))) }, body: Some(body),
        }
    }
    fn normalize_response(&self, data: &Map<String, Value>) -> Vec<SearchResultItem> {
        let chat = normalize_result_list(get_array(data.get("search_results")));
        if chat.is_empty() { normalize_result_list(get_array(data.get("results"))) } else { chat }
    }
}
