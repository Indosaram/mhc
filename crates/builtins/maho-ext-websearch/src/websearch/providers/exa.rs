use std::collections::BTreeMap;
use serde_json::{Value,json};
use super::shared::{BuildContext,BuiltSearchRequest,SearchResultItem,clamp,content_headers,json_count,normalize_results};
use crate::websearch::provider_endpoints::{SearchProvider,provider_url};
pub fn build_request(ctx:&BuildContext<'_>)->BuiltSearchRequest {
    let mut body=json!({"query":ctx.query,"numResults":json_count(clamp(ctx.max_results,1.0,20.0))});
    if let Some(domains)=ctx.allowed_domains { body["includeDomains"]=json!(domains); }
    if let Some(domains)=ctx.blocked_domains { body["excludeDomains"]=json!(domains); }
    BuiltSearchRequest{url:provider_url(SearchProvider::Exa,ctx.base_url).into(),method:"POST",headers:content_headers(BTreeMap::from([("x-api-key".into(),ctx.api_key.unwrap_or("").into())])),body}
}
pub fn normalize_response(data:&Value)->Vec<SearchResultItem> { normalize_results(data,"text",Some("snippet")) }
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn request_count_domains() { let ctx=BuildContext{query:"q",max_results:30.0,api_key:None,base_url:None,allowed_domains:Some(&[]),blocked_domains:None}; let request=build_request(&ctx); assert_eq!(request.body["numResults"],json!(20.0)); assert_eq!(request.body["includeDomains"],json!([])); }
    #[test] fn response_text_fallback() { assert_eq!(normalize_response(&json!({"results":[{"title":"t","url":"u","snippet":"s"}]}))[0].snippet.as_deref(),Some("s")); }
}
