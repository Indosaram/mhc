use std::collections::BTreeMap;
use serde_json::{Map,Value,json};
use super::{shared::*,super::{types::*,provider_endpoints::provider_url}};
pub struct ExaProvider;
impl ProviderModule for ExaProvider{
    fn build_request(&self,context:&BuildContext<'_>)->BuiltSearchRequest{
        let mut body=json!({"query":context.request.query,"numResults":clamp(context.max_results,1.0,20.0)}).as_object().cloned().unwrap_or_default();
        if let Some(domains)=&context.allowed_domains{body.insert("includeDomains".into(),json!(domains));}
        if let Some(domains)=&context.blocked_domains{body.insert("excludeDomains".into(),json!(domains));}
        BuiltSearchRequest{url:provider_url(context.config.provider,context.config.base_url.as_deref()).into(),init:SearchRequestInit{method:HttpMethod::Post,headers:content_headers(Some(&BTreeMap::from([("x-api-key".into(),context.config.api_key.clone().unwrap_or_default())])))},body:Some(body)}
    }
    fn normalize_response(&self,data:&Map<String,Value>)->Vec<SearchResultItem>{collect(get_array(data.get("results")).iter().map(|raw|{let item=get_object(Some(raw));result(get_string(item.and_then(|item|item.get("title"))),get_string(item.and_then(|item|item.get("url"))),get_string(item.and_then(|item|item.get("text"))).or_else(||get_string(item.and_then(|item|item.get("snippet")))),None,get_number(item.and_then(|item|item.get("score"))))}).collect(),50)}
}
