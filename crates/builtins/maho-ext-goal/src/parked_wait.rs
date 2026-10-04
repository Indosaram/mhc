use crate::cache_warm::{GOAL_CACHE_WARMUP_ENTRY_TYPE, GoalCacheWarmMetrics};
use maho_ext_api::SessionEntry;
use serde_json::Value;
#[derive(Clone, Debug, PartialEq)]
pub struct ParkedGoalWait { pub iteration: f64, pub delay_ms: f64, pub due_at_ms: f64, pub cache: Option<GoalCacheWarmMetrics> }
pub fn find_parked_goal_wait(entries: &[SessionEntry], goal_id: &str) -> Option<ParkedGoalWait> {
    for entry in entries.iter().rev() {
        if entry.kind == "message" || entry.kind == "custom_message" { return None; }
        if entry.kind != "custom" || entry.data.get("customType").and_then(Value::as_str) != Some(GOAL_CACHE_WARMUP_ENTRY_TYPE) { continue; }
        return parse_parked_wait(entry.data.get("data")?, goal_id);
    }
    None
}
fn number(value: &Value, field: &str) -> Option<f64> { value.get(field)?.as_f64().filter(|number| number.is_finite()) }
fn parse_parked_wait(data: &Value, goal_id: &str) -> Option<ParkedGoalWait> {
    if data.get("phase")?.as_str()? != "scheduled" || data.get("goalId")?.as_str()? != goal_id { return None; }
    let iteration = number(data,"iteration")?;
    if iteration.fract() != 0.0 || iteration < 1.0 { return None; }
    let delay_ms = number(data,"delayMs")?;
    if delay_ms <= 0.0 { return None; }
    let due_at_ms = number(data,"dueAtMs")?;
    let cache = data.get("cache").and_then(|cache| Some(GoalCacheWarmMetrics { cached_tokens: number(cache,"cachedTokens")?, ttl_seconds: number(cache,"ttlSeconds"), estimated_saved_usd: number(cache,"estimatedSavedUsd") }));
    Some(ParkedGoalWait { iteration, delay_ms, due_at_ms, cache })
}
#[cfg(test)] mod tests {
    use super::*;
    fn entry(kind: &str, data: Value) -> SessionEntry { SessionEntry { id: "e".into(), parent_id: None, timestamp: "0".into(), kind: kind.into(), data } }
    fn scheduled(goal_id: &str) -> SessionEntry { entry("custom", serde_json::json!({"customType":GOAL_CACHE_WARMUP_ENTRY_TYPE,"data":{"phase":"scheduled","goalId":goal_id,"iteration":2,"delayMs":270000,"dueAtMs":400000,"cache":{"cachedTokens":120000,"ttlSeconds":300.5}}})) }
    #[test] fn restores_parked_wait_and_fractional_cache_ttl() { let result = find_parked_goal_wait(&[scheduled("g")], "g").unwrap(); assert!((result.iteration-2.0).abs()<f64::EPSILON); assert!((result.cache.unwrap().ttl_seconds.unwrap()-300.5).abs()<f64::EPSILON); }
    #[test] fn newer_message_invalidates_wait() { let result = find_parked_goal_wait(&[scheduled("g"),entry("message",Value::Null)],"g"); assert!(result.is_none()); }
    #[test] fn newer_custom_message_invalidates_wait() { let result = find_parked_goal_wait(&[scheduled("g"),entry("custom_message",Value::Null)],"g"); assert!(result.is_none()); }
    #[test] fn latest_other_goal_wait_does_not_restore_older_one() { let result = find_parked_goal_wait(&[scheduled("g"),scheduled("other")],"g"); assert!(result.is_none()); }
    #[test] fn unrelated_custom_entry_is_skipped() { let result = find_parked_goal_wait(&[scheduled("g"),entry("custom",serde_json::json!({"customType":"other"}))],"g"); assert!(result.is_some()); }
    #[test] fn malformed_delay_invalidates_wait() { let mut entry=scheduled("g"); entry.data["data"]["delayMs"]=serde_json::json!(0); let result=find_parked_goal_wait(&[entry],"g"); assert!(result.is_none()); }
    #[test] fn fractional_iteration_invalidates_wait() { let mut entry=scheduled("g"); entry.data["data"]["iteration"]=serde_json::json!(1.5); let result=find_parked_goal_wait(&[entry],"g"); assert!(result.is_none()); }
    #[test] fn malformed_optional_cache_is_ignored() { let mut entry=scheduled("g"); entry.data["data"]["cache"]=serde_json::json!({"cachedTokens":"bad"}); let result=find_parked_goal_wait(&[entry],"g").unwrap(); assert!(result.cache.is_none()); }
}
