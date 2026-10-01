use serde::Deserialize;
use serde_json::Value;

pub const CACHE_TTL_MS: u64 = 5 * 60 * 1000;
const NOISE_FLOOR_TOKENS: f64 = 1024.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CacheMiss {
    pub missed_tokens: f64,
    pub missed_cost: f64,
    pub idle_ms: f64,
    pub model_changed: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CacheWasteTotals {
    pub missed_tokens: f64,
    pub missed_cost: f64,
    pub miss_count: usize,
}

pub trait ModelPriceSource { fn cache_read_price(&self, provider: &str, model: &str) -> Option<f64>; }
impl<F: Fn(&str, &str) -> Option<f64>> ModelPriceSource for F {
    fn cache_read_price(&self, provider: &str, model: &str) -> Option<f64> { self(provider, model) }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageCost { input: f64, cache_read: f64, cache_write: f64 }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage { input: f64, cache_read: f64, cache_write: f64, cost: UsageCost }
#[derive(Deserialize)]
struct Request { provider: String, model: String, timestamp: f64, usage: Usage }
struct PreviousRequest { prompt_tokens: f64, model_key: String, timestamp: f64, reported_cache: bool }

fn detect_miss(previous: Option<&PreviousRequest>, message: &Request, prices: &dyn ModelPriceSource) -> Option<CacheMiss> {
    let previous = previous?;
    let usage = &message.usage;
    let prompt_tokens = usage.input + usage.cache_read + usage.cache_write;
    if prompt_tokens <= 0.0 || (usage.cache_read + usage.cache_write == 0.0 && !previous.reported_cache) { return None; }
    let missed_tokens = previous.prompt_tokens.min(prompt_tokens) - usage.cache_read;
    if missed_tokens <= NOISE_FLOOR_TOKENS { return None; }
    let paid_tokens = usage.input + usage.cache_write;
    let paid_per_token = if paid_tokens > 0.0 { (usage.cost.input + usage.cost.cache_write) / paid_tokens } else { 0.0 };
    let read_per_token = if usage.cache_read > 0.0 { usage.cost.cache_read / usage.cache_read }
        else { prices.cache_read_price(&message.provider, &message.model).unwrap_or(0.0) / 1_000_000.0 };
    Some(CacheMiss { missed_tokens, missed_cost: missed_tokens * (paid_per_token - read_per_token).max(0.0),
        idle_ms: (message.timestamp - previous.timestamp).max(0.0), model_changed: format!("{}/{}", message.provider, message.model) != previous.model_key })
}

struct Scan<'a> { previous: Option<PreviousRequest>, totals: CacheWasteTotals, misses: Vec<(&'a Value, CacheMiss)> }

fn scan<'a>(entries: &'a [Value], prices: &dyn ModelPriceSource) -> Result<Scan<'a>, serde_json::Error> {
    let mut result = Scan { previous: None, totals: CacheWasteTotals::default(), misses: Vec::new() };
    for entry in entries {
        match entry.get("type").and_then(Value::as_str) {
            Some("compaction" | "branch_summary") => result.previous = None,
            Some("message") => {
                let Some(message) = entry.get("message").filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant")) else { continue; };
                let request: Request = serde_json::from_value(message.clone())?;
                if let Some(miss) = detect_miss(result.previous.as_ref(), &request, prices) {
                    result.totals.missed_tokens += miss.missed_tokens;
                    result.totals.missed_cost += miss.missed_cost;
                    result.totals.miss_count += 1;
                    result.misses.push((message, miss));
                }
                let usage = &request.usage;
                let prompt_tokens = usage.input + usage.cache_read + usage.cache_write;
                if prompt_tokens > 0.0 {
                    result.previous = Some(PreviousRequest { prompt_tokens, model_key: format!("{}/{}", request.provider, request.model), timestamp: request.timestamp,
                        reported_cache: result.previous.as_ref().is_some_and(|p| p.reported_cache) || usage.cache_read + usage.cache_write > 0.0 });
                }
            }
            _ => {}
        }
    }
    Ok(result)
}

pub fn compute_cache_waste(entries: &[Value], prices: &dyn ModelPriceSource) -> Result<CacheWasteTotals, serde_json::Error> { Ok(scan(entries, prices)?.totals) }
pub fn collect_cache_misses<'a>(entries: &'a [Value], prices: &dyn ModelPriceSource) -> Result<Vec<(&'a Value, CacheMiss)>, serde_json::Error> { Ok(scan(entries, prices)?.misses) }
pub fn detect_cache_miss(entries: &[Value], message: &Value, prices: &dyn ModelPriceSource) -> Result<Option<CacheMiss>, serde_json::Error> {
    Ok(detect_miss(scan(entries, prices)?.previous.as_ref(), &serde_json::from_value(message.clone())?, prices))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn prices(_: &str, _: &str) -> Option<f64> { Some(0.3) }
    fn message(input: f64, read: f64, write: f64, model: &str, timestamp: u64) -> Value {
        json!({"role":"assistant","provider":"test","model":model,"timestamp":timestamp,
            "usage":{"input":input,"cacheRead":read,"cacheWrite":write,"cost":{"input":input*3.0/1e6,"cacheRead":read*0.3/1e6,"cacheWrite":write*3.75/1e6}}})
    }
    fn entry(message: Value) -> Value { json!({"type":"message","message":message}) }
    fn turns() -> Vec<Value> { vec![entry(message(0.0, 0.0, 100_000.0, "m", 0)), entry(message(0.0, 100_000.0, 5_000.0, "m", 60_000))] }
    #[test]
    fn accumulates_tokens_and_cost() {
        let mut entries = turns(); entries.push(entry(message(0.0, 0.0, 110_000.0, "m", 120_000)));
        let totals = compute_cache_waste(&entries, &prices).unwrap();
        assert_eq!(totals.missed_tokens, 105_000.0);
        assert!((totals.missed_cost - 0.36225).abs() < 0.000001);
    }
    #[test]
    fn healthy_session_has_no_waste() { assert_eq!(compute_cache_waste(&turns(), &prices).unwrap(), CacheWasteTotals::default()); }
    #[test]
    fn compaction_resets_previous_request() {
        let entries = vec![turns().remove(0), json!({"type":"compaction"}), entry(message(0.0, 0.0, 20_000.0, "m", 0))];
        assert_eq!(compute_cache_waste(&entries, &prices).unwrap().missed_tokens, 0.0);
    }
    #[test]
    fn model_switches_count_misses() {
        let entries = vec![turns().remove(0), entry(message(0.0, 0.0, 100_000.0, "other", 0))];
        assert_eq!(compute_cache_waste(&entries, &prices).unwrap().miss_count, 1);
    }
    #[test]
    fn no_reported_cache_is_not_a_miss() {
        let entries = vec![entry(message(100_000.0, 0.0, 0.0, "m", 0)), entry(message(110_000.0, 0.0, 0.0, "m", 0))];
        assert_eq!(compute_cache_waste(&entries, &prices).unwrap().miss_count, 0);
    }
    #[test]
    fn collected_misses_retain_message_identity() {
        let mut entries = turns(); entries.push(entry(message(0.0, 0.0, 110_000.0, "m", 120_000)));
        let misses = collect_cache_misses(&entries, &prices).unwrap();
        assert_eq!(misses.len(), 1);
        assert!(std::ptr::eq(misses[0].0, &entries[2]["message"]));
    }
    #[test]
    fn detects_idle_duration() {
        let miss = detect_cache_miss(&turns(), &message(0.0, 0.0, 110_000.0, "m", 600_000), &prices).unwrap().unwrap();
        assert_eq!(miss.idle_ms, 540_000.0); assert!(!miss.model_changed);
    }
    #[test]
    fn detects_model_change() { assert!(detect_cache_miss(&turns(), &message(0.0, 0.0, 110_000.0, "other", 120_000), &prices).unwrap().unwrap().model_changed); }
    #[test]
    fn healthy_next_turn_is_not_a_miss() { assert!(detect_cache_miss(&turns(), &message(0.0, 105_000.0, 2_000.0, "m", 120_000), &prices).unwrap().is_none()); }
    #[test]
    fn first_turn_is_not_a_miss() { assert!(detect_cache_miss(&[], &message(0.0, 0.0, 100_000.0, "m", 0), &prices).unwrap().is_none()); }
}
