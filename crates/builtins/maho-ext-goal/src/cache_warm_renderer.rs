use crate::cache_warm::{GoalCacheWarmupEntryData,GoalCacheWarmupPhase,format_cache_ttl,format_wake_duration,format_warm_token_count,format_saved_usd};
pub fn is_same_goal_cache_warm_card(previous:Option<&GoalCacheWarmupEntryData>,next:Option<&GoalCacheWarmupEntryData>)->bool {
    next.is_some_and(|next|!next.goal_id.is_empty() && previous.is_some_and(|previous|previous.goal_id==next.goal_id))
}
pub fn title_line(data:&GoalCacheWarmupEntryData,expected_wake:&str)->String {
    let sources=if data.active_monitor_count==1.0 { "1 wake source on duty".into() } else { format!("{} wake sources on duty",maho_ai::utils::js::number_to_string(data.active_monitor_count)) };
    let iteration=data.iteration.filter(|value|value.is_finite()&&value.fract()==0.0&&*value>0.0).map_or_else(String::new,|value|format!(" · iteration {}",maho_ai::utils::js::number_to_string(value)));
    match data.phase {
        GoalCacheWarmupPhase::Scheduled=>format!("⚡ Cache-warm wait{iteration} · {sources}"),
        GoalCacheWarmupPhase::Resumed=>format!("⚡ Cache-warm wake{iteration} · {expected_wake} · {sources}"),
    }
}
pub fn warm_line(data:&GoalCacheWarmupEntryData)->Option<String> {
    let cache=data.cache.as_ref()?;
    if cache.cached_tokens<=0.0 { return None; }
    let tokens=format!("~{} tokens",format_warm_token_count(cache.cached_tokens));
    if cache.ttl_seconds.is_some_and(|ttl|data.waited_ms.unwrap_or(data.delay_ms)>=ttl*1000.0) { return Some(format!("{tokens} were cached after the prior turn · prompt-cache TTL may have elapsed before this wake")); }
    let body=match data.phase { GoalCacheWarmupPhase::Scheduled=>format!("{tokens} kept warm"),GoalCacheWarmupPhase::Resumed=>format!("{tokens} stayed warm in the prompt cache") };
    let saved=cache.estimated_saved_usd.filter(|saved|*saved>0.0).map_or_else(String::new,|saved|format!(" · est. {} saved vs a cold re-read",format_saved_usd(saved)));
    Some(format!("{body}{saved}"))
}
pub fn why_line(data:&GoalCacheWarmupEntryData,expected_wake:&str)->String {
    match data.phase {
        GoalCacheWarmupPhase::Resumed=>"Woke on schedule to keep pursuing the goal.".into(),
        GoalCacheWarmupPhase::Scheduled=>{
            let backstop=format!("Stall backstop {expected_wake}");
            match data.cache.as_ref().and_then(|cache|cache.ttl_seconds) {
                None=>format!("{backstop} - the goal resumes as soon as a wake source delivers."),
                Some(ttl) if data.delay_ms<ttl*1000.0=>format!("{backstop} - the goal resumes as soon as a wake source delivers, inside the {} prompt-cache TTL.",format_cache_ttl(ttl)),
                Some(ttl)=>format!("{backstop} - the goal resumes as soon as a wake source delivers; the {} prompt-cache TTL may elapse first.",format_cache_ttl(ttl)),
            }
        },
    }
}
pub fn expanded_line(data:&GoalCacheWarmupEntryData,expected_wake:&str)->String { format!("goal {} · planned delay {} · {expected_wake}",data.goal_id,format_wake_duration(data.delay_ms)) }
#[cfg(test)] mod tests {
    use super::*;
    fn data(id:&str)->GoalCacheWarmupEntryData { serde_json::from_value(serde_json::json!({"phase":"scheduled","goalId":id,"delayMs":1000,"activeMonitorCount":1})).unwrap() }
    #[test] fn same_nonempty_goal_id_replaces_card() { assert!(is_same_goal_cache_warm_card(Some(&data("g")),Some(&data("g")))); }
    #[test] fn missing_or_different_goal_never_replaces_card() { assert!(!is_same_goal_cache_warm_card(Some(&data("a")),Some(&data("b")))); assert!(!is_same_goal_cache_warm_card(None,Some(&data("g")))); assert!(!is_same_goal_cache_warm_card(Some(&data("")),Some(&data("")))); }
}
