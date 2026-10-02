use super::types::{RoutingStrategy,SearchProviderEntry};
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct SearchRoutingState { pub round_robin_cursor:usize,pub success_counts:Vec<usize> }
pub fn create_search_routing_state(provider_count:usize)->SearchRoutingState { SearchRoutingState{round_robin_cursor:0,success_counts:vec![0;provider_count]} }
pub fn provider_entry_label(provider:&str,id:Option<&str>,entry_id:Option<&str>)->String {
    let Some(id)=entry_id.or(id).filter(|id|!id.is_empty() && *id!=provider) else { return provider.into(); };
    if id.ends_with("/native") { id.into() } else { format!("{provider}/{}",if id.starts_with(&format!("native-{provider}-")) { "native" } else { id }) }
}
pub fn select_order(strategy:RoutingStrategy,providers:&[SearchProviderEntry],state:&mut SearchRoutingState)->Vec<usize> {
    match strategy {
        RoutingStrategy::Priority=>{
            let mut indices:Vec<_>=(0..providers.len()).collect();
            indices.sort_by(|left,right| {
                let a=providers[*left].priority.unwrap_or(*left as f64); let b=providers[*right].priority.unwrap_or(*right as f64);
                (a-b).partial_cmp(&0.).unwrap_or(std::cmp::Ordering::Equal).then(left.cmp(right))
            }); indices
        },
        RoutingStrategy::RoundRobin=>{
            let mut weighted=Vec::new(); for (index,provider) in providers.iter().enumerate() { let weight=provider.weight.unwrap_or(1.).trunc().max(1.) as usize; weighted.extend(std::iter::repeat_n(index,weight)); }
            if weighted.is_empty() { weighted.extend(0..providers.len()); }
            if weighted.is_empty() { state.round_robin_cursor=0; return vec![]; }
            let start=state.round_robin_cursor%weighted.len(); let mut order=Vec::new();
            for offset in 0..weighted.len() { let index=weighted[(start+offset)%weighted.len()]; if !order.contains(&index) { order.push(index); } }
            for index in 0..providers.len() { if !order.contains(&index) { order.push(index); } }
            state.round_robin_cursor=(state.round_robin_cursor+1)%weighted.len(); order
        },
        RoutingStrategy::FillFirst=>{
            let mut selected=0; let mut selected_count=state.success_counts.first().copied().unwrap_or(0);
            for index in 1..providers.len() { let count=state.success_counts.get(index).copied().unwrap_or(0); if count<selected_count { selected=index; selected_count=count; } }
            std::iter::once(selected).chain((0..providers.len()).filter(|index|*index!=selected)).collect()
        },
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::{SearchProviderConfig,SearchProvider};
    fn providers()->Vec<SearchProviderEntry> { (0..3).map(|_|SearchProviderEntry{config:SearchProviderConfig::new(SearchProvider::Exa),priority:None,weight:None}).collect() }
    #[test] fn priority_is_stable_with_default_index() { let mut providers=providers(); providers[2].priority=Some(-1.); assert_eq!(select_order(RoutingStrategy::Priority,&providers,&mut create_search_routing_state(3)),[2,0,1]); }
    #[test] fn weighted_round_robin_rotates_unique_providers() { let mut providers=providers(); providers[0].weight=Some(2.); let mut state=create_search_routing_state(3); assert_eq!(select_order(RoutingStrategy::RoundRobin,&providers,&mut state),[0,1,2]); assert_eq!(select_order(RoutingStrategy::RoundRobin,&providers,&mut state),[0,1,2]); assert_eq!(select_order(RoutingStrategy::RoundRobin,&providers,&mut state),[1,2,0]); }
    #[test] fn fill_first_uses_lowest_success_count() { let mut state=create_search_routing_state(3); state.success_counts=vec![2,1,0]; assert_eq!(select_order(RoutingStrategy::FillFirst,&providers(),&mut state),[2,0,1]); }
    #[test] fn native_labels_preserve_explicit_suffix() { assert_eq!(provider_entry_label("openai",Some("native-openai-1"),None),"openai/native"); assert_eq!(provider_entry_label("openai",Some("x"),Some("custom/native")),"custom/native"); }
}
