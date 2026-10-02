use std::collections::BTreeMap;
use serde_json::{Map, Value, json};
use super::{shared::*, super::{types::*, provider_endpoints::provider_url}};

pub struct KimiProvider;
impl ProviderModule for KimiProvider {
    fn build_request(&self, context: &BuildContext<'_>) -> BuiltSearchRequest {
        BuiltSearchRequest {
            url: provider_url(context.config.provider, context.config.base_url.as_deref()).into(),
            init: SearchRequestInit {
                method: HttpMethod::Post,
                headers: content_headers(Some(&BTreeMap::from([("Authorization".into(), format!("Bearer {}", context.config.api_key.as_deref().unwrap_or("")))]))),
            },
            body: Some(Map::from_iter([
                ("text_query".into(), json!(append_domain_filters(&context.request.query, context.allowed_domains.as_deref(), context.blocked_domains.as_deref()))),
                ("limit".into(), json!(clamp(context.max_results, 1.0, 20.0))),
                ("enable_page_crawling".into(), json!(false)),
                ("timeout_seconds".into(), json!(30)),
            ])),
        }
    }
    fn normalize_response(&self, data: &Map<String, Value>) -> Vec<SearchResultItem> {
        collect(get_array(data.get("search_results")).iter().map(|raw| {
            let item = get_object(Some(raw));
            result(get_string(item.and_then(|item| item.get("title"))), get_string(item.and_then(|item| item.get("url"))), get_string(item.and_then(|item| item.get("summary"))).or_else(|| get_string(item.and_then(|item| item.get("content")))), None, None)
        }).collect(), 50)
    }
}
