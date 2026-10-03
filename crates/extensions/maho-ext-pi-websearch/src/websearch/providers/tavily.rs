use std::collections::BTreeMap;
use serde_json::{Map,Value,json};
use super::{shared::*,super::{types::*,provider_endpoints::provider_url}};
pub struct TavilyProvider;
impl ProviderModule for TavilyProvider{
    fn build_request(&self,context:&BuildContext<'_>)->BuiltSearchRequest{
        let mut body=json!({"query":context.request.query,"max_results":clamp(context.max_results,1.0,20.0)}).as_object().cloned().unwrap_or_default();
        if let Some(domains)=&context.allowed_domains{body.insert("include_domains".into(),json!(domains));}
        if let Some(domains)=&context.blocked_domains{body.insert("exclude_domains".into(),json!(domains));}
        BuiltSearchRequest{url:provider_url(context.config.provider,context.config.base_url.as_deref()).into(),init:SearchRequestInit{method:HttpMethod::Post,headers:content_headers(Some(&BTreeMap::from([("Authorization".into(),format!("Bearer {}",context.config.api_key.as_deref().unwrap_or("")))])))},body:Some(body)}
    }
    fn normalize_response(&self,data:&Map<String,Value>)->Vec<SearchResultItem>{collect(get_array(data.get("results")).iter().map(|raw|{let item=get_object(Some(raw));result(get_string(item.and_then(|item|item.get("title"))),get_string(item.and_then(|item|item.get("url"))),get_string(item.and_then(|item|item.get("content"))),None,get_number(item.and_then(|item|item.get("score"))))}).collect(),50)}
}
