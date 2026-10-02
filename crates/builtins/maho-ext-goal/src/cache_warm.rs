use maho_ai::{model::Model, types::ProviderEnv, utils::prompt_cache_ttl::resolve_prompt_cache_ttl_seconds};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub const GOAL_CACHE_WARMUP_ENTRY_TYPE: &str = "goal-cache-warmup";
pub const GOAL_MONITOR_CONTINUATION_FALLBACK_DELAY_MS: f64 = 240_000.0;
pub const GOAL_MONITOR_BACKSTOP_DEFAULT_DELAY_MS: f64 = 270_000.0;
pub fn resolve_goal_monitor_continuation_delay_ms(seconds: Option<f64>) -> f64 { seconds.filter(|s| s.is_finite() && *s > 0.0).map_or(GOAL_MONITOR_BACKSTOP_DEFAULT_DELAY_MS, |s| s * 1000.0).clamp(1000.0, 3_600_000.0) }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct GoalCacheWarmMetrics { #[serde(skip_serializing_if="Option::is_none")] pub ttl_seconds: Option<f64>, pub cached_tokens: f64, #[serde(skip_serializing_if="Option::is_none")] pub estimated_saved_usd: Option<f64> }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="lowercase")]
pub enum GoalCacheWarmupPhase { Scheduled, Resumed }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct GoalCacheWarmupEntryData { pub phase: GoalCacheWarmupPhase, pub goal_id: String, #[serde(skip_serializing_if="Option::is_none")] pub iteration: Option<f64>, pub delay_ms: f64, #[serde(skip_serializing_if="Option::is_none")] pub due_at_ms: Option<f64>, #[serde(skip_serializing_if="Option::is_none")] pub waited_ms: Option<f64>, pub active_monitor_count: f64, #[serde(skip_serializing_if="Option::is_none")] pub wake_sources: Option<BTreeMap<String, f64>>, #[serde(skip_serializing_if="Option::is_none")] pub cache: Option<GoalCacheWarmMetrics> }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct GoalCacheWarmScheduleData { pub goal_id: String, pub delay_ms: f64, pub due_at_ms: f64, pub iteration: f64, pub active_monitor_count: f64, pub wake_sources: BTreeMap<String, f64>, #[serde(skip_serializing_if="Option::is_none")] pub cache: Option<GoalCacheWarmMetrics> }
pub fn create_goal_cache_warm_schedule_data(goal_id: String, delay_ms: f64, scheduled_at_ms: f64, iteration: f64, active_monitor_count: f64, wake_sources: BTreeMap<String, f64>, cache: Option<GoalCacheWarmMetrics>) -> GoalCacheWarmScheduleData { GoalCacheWarmScheduleData { goal_id, delay_ms, due_at_ms: scheduled_at_ms + delay_ms, iteration, active_monitor_count, wake_sources, cache } }
fn clamp_tokens(value: f64) -> f64 { if value.is_finite() && value > 0.0 { value.trunc() } else { 0.0 } }
pub fn estimate_cache_warm_metrics(model: Option<&Model>, env: &ProviderEnv, usage: Option<(f64, f64)>) -> Option<GoalCacheWarmMetrics> {
    let cached_tokens = usage.map_or(0.0, |(read, write)| clamp_tokens(read) + clamp_tokens(write));
    let ttl_seconds = model.and_then(|model| resolve_prompt_cache_ttl_seconds(model, Some(env))).map(|seconds| f64::from(u32::try_from(seconds).unwrap_or(u32::MAX)));
    if ttl_seconds.is_none() && cached_tokens == 0.0 { return None; }
    let estimated_saved_usd = model.filter(|_| cached_tokens > 0.0).map(|model| (model.cost.input - model.cost.cache_read).max(0.0) * cached_tokens / 1_000_000.0);
    Some(GoalCacheWarmMetrics { ttl_seconds, cached_tokens, estimated_saved_usd })
}
pub fn format_wake_duration(ms: f64) -> String { let seconds = (ms / 1000.0 + 0.5).floor(); if seconds < 60.0 { return format!("{seconds}s"); } let minutes = (seconds / 60.0).floor(); let rest_seconds = seconds % 60.0; if minutes < 60.0 { return if rest_seconds == 0.0 { format!("{minutes}m") } else { format!("{minutes}m {rest_seconds}s") }; } let hours = (minutes / 60.0).floor(); let rest_minutes = minutes % 60.0; if rest_minutes == 0.0 { format!("{hours}h") } else { format!("{hours}h {rest_minutes}m") } }
pub fn format_cache_ttl(seconds: f64) -> String { if seconds % 3600.0 == 0.0 { format!("{}h", seconds / 3600.0) } else if seconds % 60.0 == 0.0 { format!("{}m", seconds / 60.0) } else { format!("{seconds}s") } }
pub fn format_warm_token_count(tokens:f64)->String {
    let compact=|value:f64,suffix:&str| { let rendered=crate::format::fixed_decimal(value,1); format!("{}{suffix}",rendered.strip_suffix(".0").unwrap_or(&rendered)) };
    if tokens>=1_000_000.0 { compact(tokens/1_000_000.0,"M") } else if tokens>=1000.0 { compact(tokens/1000.0,"K") } else { format!("{}",tokens.trunc().max(0.0)) }
}
pub fn format_saved_usd(value: f64) -> String { if value < 0.0005 { "<$0.001".into() } else if value < 1.0 { format!("${}",crate::format::fixed_decimal(value,3)) } else { format!("${}",crate::format::fixed_decimal(value,2)) } }
#[cfg(test)] mod tests {
    use super::*;
    fn model() -> Model { serde_json::from_value(serde_json::json!({"id":"claude-cache-test","name":"Claude Cache Test","api":"anthropic-messages","provider":"anthropic","baseUrl":"https://gateway.example.invalid/v1","reasoning":false,"input":["text"],"cost":{"input":3,"output":15,"cacheRead":0.3,"cacheWrite":3.75},"contextWindow":200000,"maxTokens":8192})).unwrap() }
    #[test] fn configured_delay_clamps_and_defaults() { for (input, expected) in [(None,270000.0),(Some(3570.0),3570000.0),(Some(900.0),900000.0),(Some(5.0),5000.0),(Some(7200.0),3600000.0),(Some(0.0),270000.0),(Some(-30.0),270000.0),(Some(f64::NAN),270000.0)] { let result = resolve_goal_monitor_continuation_delay_ms(input); assert!((result-expected).abs()<f64::EPSILON); } }
    #[test] fn unknown_ttl_and_zero_cache_return_none() { let result = estimate_cache_warm_metrics(None, &ProviderEnv::new(), None); assert!(result.is_none()); }
    #[test] fn cache_without_model_has_no_savings_estimate() { let result = estimate_cache_warm_metrics(None, &ProviderEnv::new(), Some((1000.0,200.0))).unwrap(); assert!((result.cached_tokens-1200.0).abs()<f64::EPSILON); assert!(result.ttl_seconds.is_none()); assert!(result.estimated_saved_usd.is_none()); }
    #[test] fn capable_model_derives_ttl_and_savings() { let result = estimate_cache_warm_metrics(Some(&model()), &ProviderEnv::new(), Some((100000.0,20000.0))).unwrap(); assert!((result.ttl_seconds.unwrap()-300.0).abs()<f64::EPSILON); assert!((result.estimated_saved_usd.unwrap()-0.324).abs()<0.000001); }
    #[test] fn ttl_only_metrics_are_preserved() { let result = estimate_cache_warm_metrics(Some(&model()), &ProviderEnv::new(), Some((0.0,0.0))).unwrap(); assert!((result.ttl_seconds.unwrap()-300.0).abs()<f64::EPSILON); assert!(result.estimated_saved_usd.is_none()); }
    #[test] fn malformed_usage_and_negative_margin_are_clamped() { let absent = estimate_cache_warm_metrics(None, &ProviderEnv::new(), Some((-50.0,f64::NAN))); let mut model = model(); model.cost.input=0.2; model.cost.cache_read=0.5; let result = estimate_cache_warm_metrics(Some(&model), &ProviderEnv::new(), Some((1000.0,0.0))).unwrap(); assert!(absent.is_none()); assert!(result.estimated_saved_usd.unwrap().abs()<f64::EPSILON); }
    #[test] fn cached_token_and_usd_ties_match_javascript_fixed() {
        assert_eq!(format_warm_token_count(1250.0),"1.3K"); assert_eq!(format_warm_token_count(1150.0),"1.1K");
        for (value,expected) in [(0.0004,"<$0.001"),(0.0625,"$0.063"),(1.125,"$1.13"),(1.005,"$1.00"),(1e21,"$1e+21")] { assert_eq!(format_saved_usd(value),expected); }
    }
}
