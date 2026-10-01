use serde_json::Value;
use super::model_cost::ReflectionModelPricing;

#[derive(Debug)]
pub struct RegistryFallbackCandidate { pub model: String, pub cost: Option<ReflectionModelPricing> }
pub fn read_model_pricing(entry: &Value) -> Option<ReflectionModelPricing> {
    let cost = entry.as_object()?.get("cost")?.as_object()?;
    Some(ReflectionModelPricing { input: cost.get("input")?.as_f64()?, cache_read: cost.get("cacheRead").and_then(Value::as_f64) })
}
pub fn select_registry_fallback_models(available: &Value) -> Vec<RegistryFallbackCandidate> {
    let Some(entries) = available.as_array() else { return Vec::new(); };
    let mut candidates: Vec<_> = entries.iter().enumerate().filter_map(|(order, entry)| {
        let provider = entry.get("provider")?.as_str()?;
        let id = entry.get("id")?.as_str()?;
        if provider.is_empty() || id.is_empty() { return None; }
        let lower = id.to_lowercase();
        if ["embed", "image", "audio", "tts", "whisper", "rerank", "moderation"].iter().any(|word| lower.contains(word)) { return None; }
        let context = entry.get("contextWindow").and_then(Value::as_f64);
        if context.is_some_and(|context| context < 65_536.0) { return None; }
        let cost = read_model_pricing(entry);
        let fast = ["fast", "flash", "mini", "lite", "haiku", "turbo", "highspeed"].iter().any(|word| lower.contains(word));
        Some((RegistryFallbackCandidate { model: format!("{provider}/{id}"), cost }, context, fast, order))
    }).collect();
    candidates.sort_by(|(left, lc, lf, lo), (right, rc, rf, ro)| {
        match (left.cost, right.cost) {
            (Some(left), Some(right)) => left.input.total_cmp(&right.input).then_with(|| rc.unwrap_or(0.0).total_cmp(&lc.unwrap_or(0.0))).then_with(|| lo.cmp(ro)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => rf.cmp(lf).then_with(|| lo.cmp(ro)),
        }
    });
    candidates.into_iter().take(3).map(|(candidate, _, _, _)| candidate).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn priced(id: &str, input: f64, context: u32) -> Value { json!({"provider":"x", "id":id, "cost":{"input":input,"cacheRead":input / 10.0},"contextWindow":context}) }
    fn models(value: Value) -> Vec<String> { select_registry_fallback_models(&value).into_iter().map(|entry| entry.model).collect() }
    #[test]
    fn cheapest_three_win() { assert_eq!(models(json!([priced("expensive",5.0,200000),priced("cheapest",0.1,200000),priced("middle",1.0,200000),priced("fourth",2.0,200000)])), ["x/cheapest", "x/middle", "x/fourth"]); }
    #[test]
    fn larger_context_breaks_price_tie() { assert_eq!(models(json!([priced("small",1.0,128000),priced("big",1.0,1000000)]))[0], "x/big"); }
    #[test]
    fn non_chat_excluded() { assert_eq!(models(json!([priced("text-embedding-005",0.001,200000),priced("chat-model",1.0,200000)])), ["x/chat-model"]); }
    #[test]
    fn tiny_context_excluded() { assert_eq!(models(json!([priced("tiny",0.01,8192),priced("roomy",1.0,200000)])), ["x/roomy"]); }
    #[test]
    fn malformed_registry_empty() { for value in [Value::Null,json!({}),json!([{"provider":"x"},{"id":"y"},null,"junk"])] { assert!(models(value).is_empty()); } }
    #[test]
    fn priced_then_fast_unpriced() { assert_eq!(models(json!([{"provider":"local","id":"heavyweight"},{"provider":"local","id":"swift-flash"},priced("billed",9.0,200000)])), ["x/billed","local/swift-flash","local/heavyweight"]); }
    #[test]
    fn only_numeric_cost_fields_survive() { assert!(read_model_pricing(&json!({"cost":{"input":1,"cacheRead":0.1}})).is_some()); assert!(read_model_pricing(&json!({"cost":{"input":1}})).is_some()); for entry in [json!({"cost":{"cacheRead":0.1}}),json!({"cost":null}),json!({}),Value::Null] { assert!(read_model_pricing(&entry).is_none()); } }
}
