use std::collections::BTreeMap;
use serde_json::{Map,Value};
use super::super::types::*;
pub struct BuildContext<'a>{pub config:&'a SearchProviderConfig,pub request:&'a SearchRequest,pub max_results:f64,pub allowed_domains:Option<Vec<String>>,pub blocked_domains:Option<Vec<String>>}
pub trait ProviderModule{fn build_request(&self,context:&BuildContext<'_>)->BuiltSearchRequest;fn normalize_response(&self,data:&Map<String,Value>)->Vec<SearchResultItem>;}
pub fn content_headers(extra:Option<&BTreeMap<String,String>>)->BTreeMap<String,String>{let mut headers=BTreeMap::from([("Accept".into(),"application/json".into()),("Content-Type".into(),"application/json".into())]);if let Some(extra)=extra{headers.extend(extra.clone());}headers}
pub fn clamp(value:f64,min:f64,max:f64)->f64{if value.is_nan()||min.is_nan()||max.is_nan(){f64::NAN}else{value.trunc().min(max).max(min)}}
pub fn append_domain_filters(query:&str,allowed:Option<&[String]>,blocked:Option<&[String]>)->String{let mut parts=vec![query.to_owned()];for domain in allowed.unwrap_or(&[]){parts.push(format!("site:{domain}"));}for domain in blocked.unwrap_or(&[]){parts.push(format!("-site:{domain}"));}parts.join(" ")}
pub fn unique(values:&[String])->Vec<String>{let mut result=Vec::new();for value in values{if !result.contains(value){result.push(value.clone());}}result}
fn non_empty(values:Vec<String>)->Vec<String>{if values.is_empty(){vec!["invalid.invalid".into()]}else{values}}
#[derive(Debug,PartialEq,Eq)]pub struct DomainFilters{pub allowed_domains:Option<Vec<String>>,pub blocked_domains:Option<Vec<String>>}
pub fn resolve_domain_filters(config:&SearchProviderConfig,request:&SearchRequest)->DomainFilters{
    if let Some(allowed)=&config.allowed_domains{let narrowed:Vec<_>=allowed.iter().filter(|domain|request.allowed_domains.as_ref().is_none_or(|values|values.contains(domain))).filter(|domain|request.blocked_domains.as_ref().is_none_or(|values|!values.contains(domain))).cloned().collect();return DomainFilters{allowed_domains:Some(non_empty(unique(&narrowed))),blocked_domains:None};}
    if let Some(blocked)=&config.blocked_domains{let blocked=unique(&[blocked.as_slice(),request.blocked_domains.as_deref().unwrap_or(&[])].concat());if let Some(allowed)=&request.allowed_domains{return DomainFilters{allowed_domains:Some(non_empty(allowed.iter().filter(|domain|!blocked.contains(domain)).cloned().collect())),blocked_domains:None};}return DomainFilters{allowed_domains:None,blocked_domains:Some(blocked)};}
    if let Some(allowed)=&request.allowed_domains{return DomainFilters{allowed_domains:Some(unique(allowed)),blocked_domains:None};}
    DomainFilters{allowed_domains:None,blocked_domains:request.blocked_domains.as_ref().map(|values|unique(values))}
}
pub fn is_json_object(value:&Value)->bool{value.is_object()}
pub fn get_object(value:Option<&Value>)->Option<&Map<String,Value>>{value.and_then(Value::as_object)}
pub fn get_array(value:Option<&Value>)->&[Value]{value.and_then(Value::as_array).map_or(&[],Vec::as_slice)}
pub fn get_string(value:Option<&Value>)->Option<&str>{value.and_then(Value::as_str)}
pub fn get_number(value:Option<&Value>)->Option<f64>{value.and_then(Value::as_f64)}
pub fn result(title:Option<&str>,url:Option<&str>,snippet:Option<&str>,source:Option<&str>,score:Option<f64>)->Option<SearchResultItem>{Some(SearchResultItem{title:title.filter(|value|!value.is_empty())?.into(),url:url.filter(|value|!value.is_empty())?.into(),snippet:snippet.filter(|value|!value.is_empty()).map(str::to_owned),source:source.filter(|value|!value.is_empty()).map(str::to_owned),score,published_at:None})}
pub fn collect(items:Vec<Option<SearchResultItem>>,max:usize)->Vec<SearchResultItem>{items.into_iter().flatten().take(max).collect()}
pub fn parse_object_payload(value:&Value)->Map<String,Value>{value.as_object().cloned().unwrap_or_default()}
