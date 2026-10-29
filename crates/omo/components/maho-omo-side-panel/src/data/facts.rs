use serde_json::Value;
use crate::runtime::PanelHostFacts;

pub fn panel_facts_from(event: &Value, previous: &PanelHostFacts) -> PanelHostFacts {
    let Some(record) = event.as_object() else { return previous.clone(); };
    let mut facts = previous.clone();
    if let Some(model) = record.get("model") {
        let name = model.as_str()
            .or_else(|| model.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()))
            .or_else(|| model.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()));
        if let Some(name) = name { facts.model = Some(name.rsplit('/').next().unwrap_or(name).into()); }
    }
    if let Some(session) = record.get("sessionId").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        facts.session_id = Some(session.into());
    }
    if let Some(path) = record.get("goalStoreFile").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        facts.goal_store_file = Some(path.into());
    }
    let mut values = facts.value.as_object().cloned().unwrap_or_default();
    if let Some(usage) = record.get("usage").filter(|value| value.get("contextWindow").and_then(Value::as_f64).is_some()) {
        values.insert("usage".into(), serde_json::json!({
            "tokens": usage.get("tokens").and_then(Value::as_f64),
            "contextWindow": usage.get("contextWindow"),
            "percent": usage.get("percent").and_then(Value::as_f64),
        }));
    }
    if let Some(totals) = record.get("totals")
        && let (Some(input), Some(output)) = (totals.get("input").and_then(Value::as_f64), totals.get("output").and_then(Value::as_f64)) {
        let mut totals_value = serde_json::json!({
            "input": input, "output": output,
            "cacheRead": totals.get("cacheRead").and_then(Value::as_f64).unwrap_or(0.0),
            "cacheWrite": totals.get("cacheWrite").and_then(Value::as_f64).unwrap_or(0.0),
            "cost": totals.get("cost").and_then(Value::as_f64).unwrap_or(0.0),
        });
        if let Some(hit) = totals.get("latestCacheHitRate").and_then(Value::as_f64) {
            totals_value["latestCacheHitRate"] = Value::from(hit);
        }
        values.insert("totals".into(), totals_value);
    }
    facts.value = Value::Object(values);
    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_model_and_valid_usage_without_turning_missing_counts_into_zero() {
        let facts = panel_facts_from(&serde_json::json!({
            "model": {"id":"provider/family/model"}, "sessionId":"session",
            "usage":{"contextWindow":1000,"tokens":null},
            "totals":{"input":12,"output":4,"latestCacheHitRate":75},
        }), &PanelHostFacts::default());
        assert_eq!(facts.model.as_deref(), Some("model"));
        assert_eq!(facts.session_id.as_deref(), Some("session"));
        assert!(facts.value["usage"]["tokens"].is_null());
        assert_eq!(facts.value["totals"]["cacheRead"], 0.0);
        assert_eq!(facts.value["totals"]["latestCacheHitRate"], 75.0);
        let next = panel_facts_from(&serde_json::json!({"totals":{"input":"invalid"},"usage":{}}), &facts);
        assert_eq!(next.value, facts.value);
    }
}
