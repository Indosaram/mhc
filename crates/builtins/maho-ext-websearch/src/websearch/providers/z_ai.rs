use std::collections::BTreeMap;
use serde_json::{Value,json};
use super::shared::{BuildContext,BuiltSearchRequest,SearchResultItem,append_domain_filters,clamp,content_headers,json_count,result};
use crate::websearch::provider_endpoints::{provider_url,SearchProvider};
pub fn build_request(ctx:&BuildContext<'_>,model:Option<&str>,search_context_size:Option<&str>)->BuiltSearchRequest {
    let body=if let Some(model)=model.filter(|model|!model.is_empty()) {
        let mut search=json!({"enable":true,"search_engine":"search-prime","search_result":true,"count":json_count(clamp(ctx.max_results,1.,50.))});
        if let Some(domain)=ctx.allowed_domains.and_then(|domains|domains.first()).filter(|domain|!domain.is_empty()) { search["search_domain_filter"]=json!(domain); }
        if let Some(size)=search_context_size.filter(|size|!size.is_empty()) { search["content_size"]=json!(size); }
        json!({"model":model,"messages":[{"role":"user","content":ctx.query}],"tools":[{"type":"web_search","web_search":search}]})
    } else {
        let mut body=json!({"search_engine":"search-prime","search_query":append_domain_filters(ctx.query,None,ctx.blocked_domains),"count":json_count(clamp(ctx.max_results,1.,50.))});
        if let Some(domain)=ctx.allowed_domains.and_then(|domains|domains.first()).filter(|domain|!domain.is_empty()) { body["search_domain_filter"]=json!(domain); } body
    };
    BuiltSearchRequest{url:provider_url(SearchProvider::Zai,ctx.base_url).into(),method:"POST",headers:content_headers(BTreeMap::from([("Authorization".into(),format!("Bearer {}",ctx.api_key.unwrap_or_default()))])),body}
}
pub fn normalize_response(data:&Value)->Vec<SearchResultItem> {
    let normalize=|field|data.get(field).and_then(Value::as_array).into_iter().flatten().filter_map(|item|result(item.get("title").and_then(Value::as_str),item.get("link").and_then(Value::as_str),item.get("content").and_then(Value::as_str),item.get("media").and_then(Value::as_str),None)).take(50).collect::<Vec<_>>();
    let chat=normalize("web_search"); if !chat.is_empty() { chat } else { normalize("search_result") }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn direct_search_filters_blocked_and_first_allowed_domain() {
        let allowed=vec!["a.test".into(),"b.test".into()]; let blocked=vec!["x.test".into()];
        let request=build_request(&BuildContext{query:"q",max_results:100.,api_key:None,base_url:None,allowed_domains:Some(&allowed),blocked_domains:Some(&blocked)},None,None);
        assert_eq!(request.body,json!({"search_engine":"search-prime","search_query":"q -site:x.test","count":50.,"search_domain_filter":"a.test"}));
    }
    #[test] fn chat_search_prefers_chat_results() {
        let request=build_request(&BuildContext{query:"q",max_results:2.,api_key:None,base_url:None,allowed_domains:None,blocked_domains:None},Some("glm-4"),Some("high"));
        assert_eq!(request.body["tools"][0]["web_search"]["content_size"],"high");
        let response=normalize_response(&json!({"web_search":[{"title":"chat","link":"https://a","media":"a"}],"search_result":[{"title":"fallback","link":"https://b"}]})); assert_eq!(response[0].title,"chat");
    }
}
