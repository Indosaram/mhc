use std::collections::BTreeMap;
use serde_json::{Map, Value, json};
use super::{shared::*, super::{types::*, provider_endpoints::provider_url}};

pub struct SerperProvider;
impl ProviderModule for SerperProvider {
    fn build_request(&self, context: &BuildContext<'_>) -> BuiltSearchRequest {
        let body = Map::from_iter([
            ("q".into(), json!(append_domain_filters(&context.request.query, context.allowed_domains.as_deref(), context.blocked_domains.as_deref()))),
            ("num".into(), json!(clamp(context.max_results, 1.0, 20.0))),
        ]);
        BuiltSearchRequest {
            url: provider_url(context.config.provider, context.config.base_url.as_deref()).into(),
            init: SearchRequestInit {
                method: HttpMethod::Post,
                headers: content_headers(Some(&BTreeMap::from([("X-API-KEY".into(), context.config.api_key.clone().unwrap_or_default())]))),
            },
            body: Some(body),
        }
    }
    fn normalize_response(&self, data: &Map<String, Value>) -> Vec<SearchResultItem> {
        collect(get_array(data.get("organic")).iter().map(|raw| {
            let item = get_object(Some(raw));
            result(get_string(item.and_then(|item| item.get("title"))), get_string(item.and_then(|item| item.get("link"))), get_string(item.and_then(|item| item.get("snippet"))), None, None)
        }).collect(), 50)
    }
}
