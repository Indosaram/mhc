use std::collections::BTreeMap;
use serde_json::{Value,json};
use super::shared::{BuildContext,BuiltSearchRequest,SearchResultItem,append_domain_filters,clamp,content_headers,json_count,result};
use crate::websearch::provider_endpoints::{SearchProvider,provider_url};
pub fn build_request(ctx:&BuildContext<'_>)->BuiltSearchRequest {
    BuiltSearchRequest{url:provider_url(SearchProvider::Serper,ctx.base_url).into(),method:"POST",headers:content_headers(BTreeMap::from([("X-API-KEY".into(),ctx.api_key.unwrap_or("").into())])),body:json!({"q":append_domain_filters(ctx.query,ctx.allowed_domains,ctx.blocked_domains),"num":json_count(clamp(ctx.max_results,1.0,20.0))})}
}
pub fn normalize_response(data:&Value)->Vec<SearchResultItem> {
    data.get("organic").and_then(Value::as_array).into_iter().flatten().filter_map(|item|result(item.get("title").and_then(Value::as_str),item.get("link").and_then(Value::as_str),item.get("snippet").and_then(Value::as_str),None,None)).take(50).collect()
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn request_clamps_results_and_filters_domains() { let ctx=BuildContext{query:"q",max_results:0.0,api_key:None,base_url:None,allowed_domains:None,blocked_domains:Some(&["blocked.org".into()])}; let request=build_request(&ctx); assert_eq!(request.body["q"],"q -site:blocked.org"); assert_eq!(request.body["num"],json!(1.0)); }
    #[test] fn response_uses_organic_links() { let results=normalize_response(&json!({"organic":[{"title":"t","link":"u","snippet":"s"},null]})); assert_eq!(results.len(),1); assert_eq!(results[0].url,"u"); }
}
