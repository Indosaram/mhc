use serde_json::{Value,json};
use super::{types::{SearchRequest,WebsearchConfig,RoutingStrategy},search::{SearchRoutingState,create_search_routing_state}};
pub fn parameters()->Value { json!({"type":"object","properties":{"query":{"type":"string","minLength":2,"description":"The search query to use"},"allowed_domains":{"type":"array","items":{"type":"string"},"description":"Only include search results from these domains"},"blocked_domains":{"type":"array","items":{"type":"string"},"description":"Never include search results from these domains"}},"required":["query"],"additionalProperties":false}) }
pub fn request_from_arguments(query:String,allowed_domains:Option<Vec<String>>,blocked_domains:Option<Vec<String>>,config:&WebsearchConfig)->Result<SearchRequest,String> {
    if allowed_domains.as_ref().is_some_and(|domains|!domains.is_empty()) && blocked_domains.as_ref().is_some_and(|domains|!domains.is_empty()) { return Err("Error: Cannot specify both allowed_domains and blocked_domains in the same request".into()); }
    Ok(SearchRequest{query,max_results:config.providers.first().and_then(|entry|entry.config.max_results).unwrap_or(10.),allowed_domains,blocked_domains})
}
pub fn routing_key(config:&WebsearchConfig)->String {
    let strategy=match config.strategy { RoutingStrategy::Priority=>"priority",RoutingStrategy::RoundRobin=>"round-robin",RoutingStrategy::FillFirst=>"fill-first" };
    format!("{strategy}:{}",config.providers.iter().map(|entry|entry.config.id.as_deref().unwrap_or(entry.config.provider.as_str())).collect::<Vec<_>>().join("|"))
}
pub fn sync_routing_state(config:&WebsearchConfig,key:&mut String,state:&mut Option<SearchRoutingState>) {
    let next=routing_key(config);
    if state.as_ref().is_none_or(|state|state.success_counts.len()!=config.providers.len()) || *key!=next { *state=Some(create_search_routing_state(config.providers.len())); *key=next; }
}
pub fn format_search_progress_text(query:&str,provider_labels:&[String],current_provider:Option<&str>)->String {
    let route=if let Some(provider)=current_provider.filter(|provider|!provider.is_empty()) { provider.into() } else if provider_labels.is_empty() { "configured providers".into() } else { provider_labels.join(" -> ") };
    format!("Searching \"{query}\" via {route}")
}
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::{SearchProvider,SearchProviderConfig,SearchProviderEntry};
    fn config()->WebsearchConfig { WebsearchConfig{strategy:RoutingStrategy::Priority,fallback:true,auto:true,providers:vec![SearchProviderEntry{config:SearchProviderConfig::new(SearchProvider::Exa),priority:None,weight:None}]} }
    #[test] fn conflicting_nonempty_lists_fail_before_search() { assert!(request_from_arguments("q".into(),Some(vec!["a".into()]),Some(vec!["b".into()]),&config()).is_err()); assert!(request_from_arguments("q".into(),Some(vec![]),Some(vec!["b".into()]),&config()).is_ok()); }
    #[test] fn routing_state_only_resets_on_identity_or_count_change() { let mut config=config(); let mut key=String::new(); let mut state=None; sync_routing_state(&config,&mut key,&mut state); state.as_mut().unwrap().success_counts[0]=2; config.providers[0].config.model=Some("new".into()); sync_routing_state(&config,&mut key,&mut state); assert_eq!(state.as_ref().unwrap().success_counts,[2]); config.providers[0].config.id=Some("second".into()); sync_routing_state(&config,&mut key,&mut state); assert_eq!(state.unwrap().success_counts,[0]); }
    #[test] fn query_schema_and_default_limit() { assert_eq!(parameters()["properties"]["query"]["minLength"],2); assert_eq!(parameters()["additionalProperties"],false); assert_eq!(request_from_arguments("q".into(),None,None,&config()).unwrap().max_results,10.); }
}
