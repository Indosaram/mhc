use std::collections::BTreeMap;
use serde_json::{Value,json};
use super::{shared::{BuildContext,BuiltSearchRequest,SearchResultItem,content_headers},openai_responses::normalize_responses_payload};
use crate::websearch::provider_endpoints::{provider_url,SearchProvider};
pub fn build_request(ctx:&BuildContext<'_>,model:Option<&str>)->BuiltSearchRequest {
    let mut tool=json!({"type":"web_search"});
    if let Some(domains)=ctx.allowed_domains { tool["filters"]=json!({"allowed_domains":domains.iter().take(5).collect::<Vec<_>>()}); }
    else if let Some(domains)=ctx.blocked_domains { tool["filters"]=json!({"excluded_domains":domains.iter().take(5).collect::<Vec<_>>()}); }
    BuiltSearchRequest{url:provider_url(SearchProvider::Xai,ctx.base_url).into(),method:"POST",headers:content_headers(BTreeMap::from([("Authorization".into(),format!("Bearer {}",ctx.api_key.unwrap_or_default()))])),body:json!({"model":model.unwrap_or("grok-4.3"),"input":ctx.query,"tools":[tool],"tool_choice":"required"})}
}
pub fn normalize_response(data:&Value)->Vec<SearchResultItem> { normalize_responses_payload(data,true) }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn filters_are_capped_with_allowed_precedence() { let domains:Vec<_>=(0..8).map(|i|format!("d{i}")).collect(); let req=build_request(&BuildContext{query:"q",max_results:1.,api_key:None,base_url:None,allowed_domains:Some(&domains),blocked_domains:Some(&domains)},None); assert_eq!(req.body["tools"][0]["filters"]["allowed_domains"].as_array().unwrap().len(),5); assert!(req.body["tools"][0]["filters"].get("excluded_domains").is_none()); }
    #[test] fn citation_fallback_enabled() { assert_eq!(normalize_response(&json!({"citations":["https://a"]}))[0].url,"https://a"); }
}
