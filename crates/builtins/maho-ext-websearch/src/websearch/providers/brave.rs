use std::collections::BTreeMap;
use serde_json::Value;
use super::shared::{BuildContext,BuiltSearchRequest,SearchResultItem,append_domain_filters,clamp,result};
use crate::websearch::provider_endpoints::{SearchProvider,provider_url};
pub fn build_request(ctx:&BuildContext<'_>)->Result<BuiltSearchRequest,url::ParseError> {
    let mut url=url::Url::parse(provider_url(SearchProvider::Brave,ctx.base_url))?;
    url.query_pairs_mut().append_pair("q",&append_domain_filters(ctx.query,ctx.allowed_domains,ctx.blocked_domains)).append_pair("count",&clamp(ctx.max_results,1.0,20.0).to_string());
    Ok(BuiltSearchRequest{url:url.into(),method:"GET",headers:BTreeMap::from([("Accept".into(),"application/json".into()),("X-Subscription-Token".into(),ctx.api_key.unwrap_or("").into())]),body:Value::Null})
}
pub fn normalize_response(data:&Value)->Vec<SearchResultItem> {
    data.get("web").and_then(|web|web.get("results")).and_then(Value::as_array).into_iter().flatten().filter_map(|item|result(item.get("title").and_then(Value::as_str),item.get("url").and_then(Value::as_str),item.get("description").and_then(Value::as_str),None,None)).take(50).collect()
}
#[cfg(test)] mod tests {
    use super::*; use serde_json::json;
    #[test] fn query_filters_and_count_are_encoded() { let ctx=BuildContext{query:"rust api",max_results:40.0,api_key:None,base_url:None,allowed_domains:Some(&["example.org".into()]),blocked_domains:None}; let request=build_request(&ctx).unwrap(); let url=url::Url::parse(&request.url).unwrap(); assert_eq!(url.query_pairs().find(|(k,_)|k=="q").unwrap().1,"rust api site:example.org"); assert_eq!(url.query_pairs().find(|(k,_)|k=="count").unwrap().1,"20"); }
    #[test] fn response_normalization_drops_missing_title() { let results=normalize_response(&json!({"web":{"results":[{"title":"t","url":"u","description":"s"},{"url":"bad"}]}})); assert_eq!(results.len(),1); assert_eq!(results[0].snippet.as_deref(),Some("s")); }
}
