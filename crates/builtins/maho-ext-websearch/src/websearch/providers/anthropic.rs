use std::collections::BTreeMap;
use serde_json::{Value,json};
use super::shared::{BuildContext,BuiltSearchRequest,SearchResultItem,content_headers,result};
use crate::websearch::provider_endpoints::{provider_url,SearchProvider};
pub fn build_anthropic_messages_search_request(ctx:&BuildContext<'_>,provider:SearchProvider,model:Option<&str>,default_model:&str)->BuiltSearchRequest {
    let mut tool=json!({"type":"web_search_20250305","name":"web_search","max_uses":8});
    if let Some(domains)=ctx.allowed_domains { tool["allowed_domains"]=json!(domains); }
    if let Some(domains)=ctx.blocked_domains { tool["blocked_domains"]=json!(domains); }
    BuiltSearchRequest{url:provider_url(provider,ctx.base_url).into(),method:"POST",headers:content_headers(BTreeMap::from([("x-api-key".into(),ctx.api_key.unwrap_or_default().into()),("anthropic-version".into(),"2023-06-01".into())])),body:json!({"model":model.unwrap_or(default_model),"max_tokens":1024,"messages":[{"role":"user","content":ctx.query}],"tools":[tool]})}
}
pub fn build_request(ctx:&BuildContext<'_>,model:Option<&str>)->BuiltSearchRequest { build_anthropic_messages_search_request(ctx,SearchProvider::Anthropic,model,"claude-sonnet-4-5-20250929") }
pub fn normalize_anthropic_messages_search_payload(data:&Value)->Vec<SearchResultItem> {
    let content=data.get("content").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let text=content.iter().filter_map(|item|item.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n");
    content.iter().filter(|item|item.get("type").and_then(Value::as_str)==Some("web_search_tool_result")).flat_map(|item|item.get("content").and_then(Value::as_array).into_iter().flatten()).filter_map(|item|result(item.get("title").and_then(Value::as_str),item.get("url").and_then(Value::as_str),Some(item.get("page_age").and_then(Value::as_str).unwrap_or(&text)),None,None)).take(50).collect()
}
pub use normalize_anthropic_messages_search_payload as normalize_response;
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn model_and_domain_fields_preserve_empty_values() {
        let ctx=BuildContext{query:"q",max_results:1.,api_key:None,base_url:None,allowed_domains:Some(&[]),blocked_domains:Some(&[])}; let req=build_request(&ctx,Some("")); assert_eq!(req.body["model"],""); assert_eq!(req.body["tools"][0]["allowed_domains"],json!([])); assert_eq!(req.headers["anthropic-version"],"2023-06-01");
    }
    #[test] fn page_age_precedes_joined_assistant_text() {
        let results=normalize_response(&json!({"content":[{"text":"first"},{"text":"second"},{"type":"web_search_tool_result","content":[{"title":"a","url":"u"},{"title":"b","url":"v","page_age":""}]}]})); assert_eq!(results[0].snippet.as_deref(),Some("first\nsecond")); assert_eq!(results[1].snippet,None);
    }
}
