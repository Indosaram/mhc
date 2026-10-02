use std::collections::BTreeMap;
use serde_json::{Map,Value,json};
use super::{shared::*,openai_responses::normalize_responses_payload,super::{types::*,provider_endpoints::provider_url}};
pub struct XaiProvider;
impl ProviderModule for XaiProvider{
    fn build_request(&self,context:&BuildContext<'_>)->BuiltSearchRequest{
        let mut tool=Map::from_iter([("type".into(),json!("web_search"))]);
        if let Some(domains)=&context.allowed_domains{tool.insert("filters".into(),json!({"allowed_domains":domains.iter().take(5).collect::<Vec<_>>()}));}else if let Some(domains)=&context.blocked_domains{tool.insert("filters".into(),json!({"excluded_domains":domains.iter().take(5).collect::<Vec<_>>()}));}
        BuiltSearchRequest{url:provider_url(context.config.provider,context.config.base_url.as_deref()).into(),init:SearchRequestInit{method:HttpMethod::Post,headers:content_headers(Some(&BTreeMap::from([("Authorization".into(),format!("Bearer {}",context.config.api_key.as_deref().unwrap_or("")))])))},body:Some(Map::from_iter([("model".into(),json!(context.config.model.as_deref().unwrap_or("grok-4.3"))),("input".into(),json!(context.request.query)),("tools".into(),json!([tool])),("tool_choice".into(),json!("required"))]))}
    }
    fn normalize_response(&self,data:&Map<String,Value>)->Vec<SearchResultItem>{normalize_responses_payload(data,true)}
}
