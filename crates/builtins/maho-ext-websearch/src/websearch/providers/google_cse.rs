use std::collections::BTreeMap;
use serde_json::Value;
use super::shared::{BuildContext,BuiltSearchRequest,SearchResultItem,append_domain_filters,clamp,result};
use crate::websearch::provider_endpoints::{SearchProvider,provider_url};
pub fn build_request(ctx:&BuildContext<'_>,search_engine_id:Option<&str>)->Result<BuiltSearchRequest,url::ParseError> {
    let mut url=url::Url::parse(provider_url(SearchProvider::GoogleCse,ctx.base_url))?;
    url.query_pairs_mut().append_pair("q",&append_domain_filters(ctx.query,ctx.allowed_domains,ctx.blocked_domains)).append_pair("key",ctx.api_key.unwrap_or("")).append_pair("cx",search_engine_id.unwrap_or("")).append_pair("num",&clamp(ctx.max_results,1.0,10.0).to_string());
    Ok(BuiltSearchRequest{url:url.into(),method:"GET",headers:BTreeMap::from([("Accept".into(),"application/json".into())]),body:Value::Null})
}
pub fn normalize_response(data:&Value)->Vec<SearchResultItem> {
    data.get("items").and_then(Value::as_array).into_iter().flatten().filter_map(|item|result(item.get("title").and_then(Value::as_str),item.get("link").and_then(Value::as_str),item.get("snippet").and_then(Value::as_str),None,None)).take(50).collect()
}
#[cfg(test)] mod tests {
    use super::*; use serde_json::json;
    #[test] fn count_and_engine_are_encoded() { let ctx=BuildContext{query:"q",max_results:30.0,api_key:None,base_url:None,allowed_domains:None,blocked_domains:None}; let request=build_request(&ctx,Some("engine")).unwrap(); let url=url::Url::parse(&request.url).unwrap(); assert_eq!(url.query_pairs().find(|(k,_)|k=="cx").unwrap().1,"engine"); assert_eq!(url.query_pairs().find(|(k,_)|k=="num").unwrap().1,"10"); }
    #[test] fn response_uses_item_links() { assert_eq!(normalize_response(&json!({"items":[{"title":"t","link":"u"}]}))[0].url,"u"); }
}
