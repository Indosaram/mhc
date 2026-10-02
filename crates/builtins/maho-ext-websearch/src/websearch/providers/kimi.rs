use std::collections::BTreeMap;
use serde_json::{Value,json};
use super::shared::{BuildContext,BuiltSearchRequest,SearchResultItem,append_domain_filters,clamp,content_headers,json_count,result};
use crate::websearch::provider_endpoints::{provider_url,SearchProvider};
pub fn build_request(ctx:&BuildContext<'_>)->BuiltSearchRequest {
    BuiltSearchRequest{url:provider_url(SearchProvider::Kimi,ctx.base_url).into(),method:"POST",headers:content_headers(BTreeMap::from([("Authorization".into(),format!("Bearer {}",ctx.api_key.unwrap_or_default()))])),body:json!({"text_query":append_domain_filters(ctx.query,ctx.allowed_domains,ctx.blocked_domains),"limit":json_count(clamp(ctx.max_results,1.,20.)),"enable_page_crawling":false,"timeout_seconds":30})}
}
pub fn normalize_response(data:&Value)->Vec<SearchResultItem> {
    data.get("search_results").and_then(Value::as_array).into_iter().flatten().filter_map(|item|result(item.get("title").and_then(Value::as_str),item.get("url").and_then(Value::as_str),item.get("summary").and_then(Value::as_str).or_else(||item.get("content").and_then(Value::as_str)),None,None)).take(50).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn request_filters_and_limits() {
        let allowed=vec!["a".into()]; let blocked=vec!["b".into()]; let req=build_request(&BuildContext{query:"q",max_results:0.,api_key:None,base_url:None,allowed_domains:Some(&allowed),blocked_domains:Some(&blocked)}); assert_eq!(req.body["text_query"],"q site:a -site:b"); assert_eq!(req.body["limit"],1.); assert_eq!(req.body["enable_page_crawling"],false);
    }
    #[test] fn summary_precedence_and_content_fallback() {
        let results=normalize_response(&json!({"search_results":[{"title":"a","url":"u","summary":"","content":"unused"},{"title":"b","url":"v","content":"body"}]})); assert_eq!(results[0].snippet,None); assert_eq!(results[1].snippet.as_deref(),Some("body"));
    }
}
