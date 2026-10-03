use std::collections::BTreeMap;
use serde_json::{Value,json};
#[derive(Clone,Debug,PartialEq)]
pub struct SearchResultItem { pub title:String,pub url:String,pub snippet:Option<String>,pub source:Option<String>,pub score:Option<f64>,pub published_at:Option<String> }
#[derive(Clone,Debug,PartialEq)]
pub struct BuiltSearchRequest { pub url:String,pub method:&'static str,pub headers:BTreeMap<String,String>,pub body:Value }
pub struct BuildContext<'a> { pub query:&'a str,pub max_results:f64,pub api_key:Option<&'a str>,pub base_url:Option<&'a str>,pub allowed_domains:Option<&'a [String]>,pub blocked_domains:Option<&'a [String]> }
pub fn content_headers(extra:BTreeMap<String,String>)->BTreeMap<String,String> { let mut headers=BTreeMap::from([("Accept".into(),"application/json".into()),("Content-Type".into(),"application/json".into())]); headers.extend(extra); headers }
pub fn clamp(value:f64,min:f64,max:f64)->f64 { if value.is_nan() || min.is_nan() || max.is_nan() { f64::NAN } else { value.trunc().min(max).max(min) } }
pub fn append_domain_filters(query:&str,allowed:Option<&[String]>,blocked:Option<&[String]>)->String { let mut parts=vec![query.to_owned()]; for domain in allowed.unwrap_or_default() { parts.push(format!("site:{domain}")); } for domain in blocked.unwrap_or_default() { parts.push(format!("-site:{domain}")); } parts.join(" ") }
pub fn unique(values:&[String])->Vec<String> { let mut result=Vec::new(); for value in values { if !result.contains(value) { result.push(value.clone()); } } result }
fn non_empty(values:Vec<String>)->Vec<String> { if values.is_empty() { vec!["invalid.invalid".into()] } else { values } }
pub fn resolve_domain_filters(config_allowed:Option<&[String]>,config_blocked:Option<&[String]>,request_allowed:Option<&[String]>,request_blocked:Option<&[String]>)->(Option<Vec<String>>,Option<Vec<String>>) {
    if let Some(allowed)=config_allowed {
        let allowed:Vec<_>=allowed.iter().filter(|d|request_allowed.is_none_or(|r|r.contains(d))).filter(|d|request_blocked.is_none_or(|r|!r.contains(d))).cloned().collect();
        return (Some(non_empty(unique(&allowed))),None);
    }
    if let Some(blocked)=config_blocked {
        let blocked=unique(&blocked.iter().chain(request_blocked.unwrap_or_default()).cloned().collect::<Vec<_>>());
        if let Some(allowed)=request_allowed { return (Some(non_empty(allowed.iter().filter(|d|!blocked.contains(d)).cloned().collect())),None); }
        return (None,Some(blocked));
    }
    if let Some(allowed)=request_allowed { return (Some(unique(allowed)),None); }
    if let Some(blocked)=request_blocked { return (None,Some(unique(blocked))); }
    (None,None)
}
pub fn result(title:Option<&str>,url:Option<&str>,snippet:Option<&str>,source:Option<&str>,score:Option<f64>)->Option<SearchResultItem> {
    let title=title.filter(|s|!s.is_empty())?; let url=url.filter(|s|!s.is_empty())?;
    Some(SearchResultItem{title:title.into(),url:url.into(),snippet:snippet.filter(|s|!s.is_empty()).map(str::to_owned),source:source.filter(|s|!s.is_empty()).map(str::to_owned),score,published_at:None})
}
pub fn normalize_results(data:&Value,text_field:&str,fallback:Option<&str>)->Vec<SearchResultItem> {
    data.get("results").and_then(Value::as_array).into_iter().flatten().filter_map(|item|result(item.get("title").and_then(Value::as_str),item.get("url").and_then(Value::as_str),item.get(text_field).and_then(Value::as_str).or_else(||fallback.and_then(|f|item.get(f).and_then(Value::as_str))),None,item.get("score").and_then(Value::as_f64))).take(50).collect()
}
pub fn json_count(count:f64)->Value { if count.is_finite() { json!(count) } else { Value::Null } }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn configured_allowlist_cannot_expand() { let a=vec!["a".into()]; let b=vec!["b".into()]; assert_eq!(resolve_domain_filters(Some(&a),None,Some(&b),None),(Some(vec!["invalid.invalid".into()]),None)); }
    #[test] fn configured_blocklist_narrows_requested_allowlist() { let blocked=vec!["a".into()]; let allowed=vec!["a".into(),"b".into()]; assert_eq!(resolve_domain_filters(None,Some(&blocked),Some(&allowed),None),(Some(vec!["b".into()]),None)); }
    #[test] fn empty_request_allowlist_stays_empty() { assert_eq!(resolve_domain_filters(None,None,Some(&[]),None),(Some(vec![]),None)); }
    #[test] fn preserves_unique_order() { assert_eq!(unique(&["b".into(),"a".into(),"b".into()]),["b","a"]); }
    #[test] fn domain_query_filters() { assert_eq!(append_domain_filters("q",Some(&["a".into()]),Some(&["b".into()])),"q site:a -site:b"); }
    #[test] fn invalid_results_dropped() { assert!(result(Some(""),Some("url"),None,None,None).is_none()); assert!(result(Some("title"),None,None,None,None).is_none()); }
}
