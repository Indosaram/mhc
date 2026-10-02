use std::collections::BTreeMap;
use serde_json::{Map, Value, json};
use super::{shared::*, super::{types::*, provider_endpoints::provider_url}};

pub struct ZAiProvider;
impl ProviderModule for ZAiProvider {
    fn build_request(&self, context: &BuildContext<'_>) -> BuiltSearchRequest {
        let first_domain = context.allowed_domains.as_ref().and_then(|domains| domains.first()).filter(|domain| !domain.is_empty());
        let body = if let Some(model) = context.config.model.as_deref().filter(|model| !model.is_empty()) {
            let mut search = Map::from_iter([("enable".into(), json!(true)), ("search_engine".into(), json!("search-prime")), ("search_result".into(), json!(true)), ("count".into(), json!(clamp(context.max_results, 1.0, 50.0)))]);
            if let Some(domain) = first_domain { search.insert("search_domain_filter".into(), json!(domain)); }
            if let Some(size) = context.config.search_context_size { search.insert("content_size".into(), json!(size)); }
            Map::from_iter([("model".into(), json!(model)), ("messages".into(), json!([{ "role":"user", "content":context.request.query }])), ("tools".into(), json!([{ "type":"web_search", "web_search":search }]))])
        } else {
            let mut body = Map::from_iter([("search_engine".into(), json!("search-prime")), ("search_query".into(), json!(append_domain_filters(&context.request.query, None, context.blocked_domains.as_deref()))), ("count".into(), json!(clamp(context.max_results, 1.0, 50.0)))]);
            if let Some(domain) = first_domain { body.insert("search_domain_filter".into(), json!(domain)); }
            body
        };
        BuiltSearchRequest { url: provider_url(context.config.provider, context.config.base_url.as_deref()).into(), init: SearchRequestInit { method: HttpMethod::Post, headers: content_headers(Some(&BTreeMap::from([("Authorization".into(), format!("Bearer {}", context.config.api_key.as_deref().unwrap_or("")))]))) }, body: Some(body) }
    }
    fn normalize_response(&self, data: &Map<String, Value>) -> Vec<SearchResultItem> {
        let normalize = |key| collect(get_array(data.get(key)).iter().map(|raw| {
            let item = get_object(Some(raw))?;
            result(get_string(item.get("title")), get_string(item.get("link")), get_string(item.get("content")), get_string(item.get("media")), None)
        }).collect(), 50);
        let chat = normalize("web_search");
        if chat.is_empty() { normalize("search_result") } else { chat }
    }
}
