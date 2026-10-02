use std::collections::BTreeMap;
use serde_json::{Value,json};
use super::shared::{BuildContext,BuiltSearchRequest,SearchResultItem,clamp,content_headers,json_count,normalize_results};
use crate::websearch::provider_endpoints::{SearchProvider,provider_url};
pub fn build_request(ctx:&BuildContext<'_>)->BuiltSearchRequest {
    let mut body=json!({"query":ctx.query,"max_results":json_count(clamp(ctx.max_results,1.0,20.0))});
    if let Some(domains)=ctx.allowed_domains { body["include_domains"]=json!(domains); }
    if let Some(domains)=ctx.blocked_domains { body["exclude_domains"]=json!(domains); }
    BuiltSearchRequest{url:provider_url(SearchProvider::Tavily,ctx.base_url).into(),method:"POST",headers:content_headers(BTreeMap::from([("Authorization".into(),format!("Bearer {}",ctx.api_key.unwrap_or("")))])),body}
}
pub fn normalize_response(data:&Value)->Vec<SearchResultItem> { normalize_results(data,"content",None) }
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn request_minimum() { let ctx=BuildContext{query:"q",max_results:0.0,api_key:None,base_url:None,allowed_domains:None,blocked_domains:None}; assert_eq!(build_request(&ctx).body["max_results"],json!(1.0)); }
    #[test] fn response_missing_items() { assert!(normalize_response(&json!({"results":[{"title":"t"},null]})).is_empty()); }
}
