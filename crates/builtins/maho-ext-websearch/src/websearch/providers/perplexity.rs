use std::collections::BTreeMap;
use serde_json::{Value,json};
use super::shared::{BuildContext,BuiltSearchRequest,SearchResultItem,clamp,content_headers,json_count,result};
use crate::websearch::provider_endpoints::{provider_url,SearchProvider};
pub fn build_request(ctx:&BuildContext<'_>,model:Option<&str>,search_context_size:Option<&str>)->BuiltSearchRequest {
    let mut body=if let Some(model)=model.filter(|model|!model.is_empty()) { json!({"model":model,"messages":[{"role":"user","content":ctx.query}]}) } else { json!({"query":ctx.query,"max_results":json_count(clamp(ctx.max_results,1.,20.))}) };
    if let Some(domains)=ctx.allowed_domains { body["search_domain_filter"]=json!(domains); }
    else if let Some(domains)=ctx.blocked_domains { body["search_domain_filter"]=json!(domains.iter().map(|domain|format!("-{domain}")).collect::<Vec<_>>()); }
    if model.is_some_and(|model|!model.is_empty()) && let Some(size)=search_context_size.filter(|size|!size.is_empty()) { body["web_search_options"]=json!({"search_context_size":size}); }
    BuiltSearchRequest{url:provider_url(SearchProvider::Perplexity,ctx.base_url).into(),method:"POST",headers:content_headers(BTreeMap::from([("Authorization".into(),format!("Bearer {}",ctx.api_key.unwrap_or_default()))])),body}
}
pub fn normalize_response(data:&Value)->Vec<SearchResultItem> {
    let normalize=|field|data.get(field).and_then(Value::as_array).into_iter().flatten().filter_map(|item| {
        let mut search=result(item.get("title").and_then(Value::as_str),item.get("url").and_then(Value::as_str),item.get("snippet").and_then(Value::as_str),None,None)?;
        search.published_at=item.get("date").and_then(Value::as_str).or_else(||item.get("last_updated").and_then(Value::as_str)).filter(|date|!date.is_empty()).map(str::to_owned); Some(search)
    }).take(50).collect::<Vec<_>>();
    let chat=normalize("search_results"); if !chat.is_empty() { chat } else { normalize("results") }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn allowed_domain_precedence_and_model_options() {
        let blocked=vec!["blocked".into()]; let ctx=BuildContext{query:"q",max_results:30.,api_key:None,base_url:None,allowed_domains:Some(&[]),blocked_domains:Some(&blocked)};
        let req=build_request(&ctx,Some("sonar"),Some("high")); assert_eq!(req.body["search_domain_filter"],json!([])); assert_eq!(req.body["web_search_options"]["search_context_size"],"high");
        let req=build_request(&ctx,None,Some("high")); assert_eq!(req.body["max_results"],20.); assert!(req.body.get("web_search_options").is_none());
    }
    #[test] fn fallback_results_preserve_publication_date() {
        let results=normalize_response(&json!({"search_results":[{"title":"invalid"}],"results":[{"title":"ok","url":"https://a","last_updated":"2026-01-01"}]})); assert_eq!(results[0].published_at.as_deref(),Some("2026-01-01"));
    }
}
