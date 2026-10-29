use serde_json::{Value, json};

pub fn resolve_memory_settings(settings: Option<&Value>) -> Result<Value, String> {
    if let Some(settings) = settings { return Ok(settings.clone()); }
    let mut issues = vec![];
    omo_config_core::internal::validate::parse(&omo_config_core::schema::omo_memory_settings_schema(), &serde_json::json!({}), &[], &mut issues).ok_or_else(|| format!("memory settings defaults failed: {issues:?}"))
}

pub fn resolve_agent_reflection_settings(settings: Option<&Value>, agent_id: &str) -> Result<Value, String> {
    let resolved = resolve_memory_settings(settings)?;
    let mut reflection = resolved["reflection"].as_object().cloned().ok_or_else(|| "memory reflection settings must be an object".to_owned())?;
    let overrides = resolved["agents"][agent_id]["reflection"].as_object();
    let mut trigger = reflection.get("trigger").and_then(Value::as_object).cloned().unwrap_or_default();
    if let Some(overrides) = overrides {
        reflection.extend(overrides.clone());
        if let Some(overrides) = overrides.get("trigger").and_then(Value::as_object) { trigger.extend(overrides.clone()); }
    }
    reflection.insert("trigger".into(), Value::Object(trigger));
    Ok(Value::Object(reflection))
}

/// Pinned `memory.recall` defaults, used until the config schema materializes the block.
fn default_recall_block() -> Value {
    json!({
        "enabled": true,
        "max_items": 2,
        "category": "quick",
        "event_caps": { "tool_args": 400, "result_head": 600, "assistant": 1500, "prompt": 4000 },
        "sidecar_max_tokens": 48000,
        "max_concurrent_wakes": 2,
        "tool_budget": 8,
        "query_expansion": false,
    })
}

/// Pinned `memory.write_notice` defaults, used until the config schema materializes the block.
fn default_write_notice_block() -> Value {
    json!({ "enabled": true })
}

/// The resolved block when the settings carry one, else the pinned default.
fn settings_block(resolved: &Value, key: &str, fallback: Value) -> Value {
    match resolved.get(key) {
        Some(Value::Object(_)) => resolved[key].clone(),
        _ => fallback,
    }
}

/// Base `memory.recall` block under the bound agent's layer override (pin `recall-wiring.ts:57`).
///
/// `event_caps` merges per field, so an agent that tightens one cap keeps the root's other three.
pub fn resolve_agent_recall_settings(
    settings: Option<&Value>,
    agent_id: &str,
) -> Result<Value, String> {
    let resolved = resolve_memory_settings(settings)?;
    let mut merged = default_recall_block().as_object().cloned().unwrap_or_default();
    let mut event_caps = merged.get("event_caps").and_then(Value::as_object).cloned().unwrap_or_default();
    merge_recall_layer(&mut merged, &mut event_caps, resolved.get("recall"));
    let agent = resolved
        .get("agents")
        .and_then(|agents| agents.get(agent_id))
        .and_then(|agent| agent.get("recall"));
    merge_recall_layer(&mut merged, &mut event_caps, agent);
    merged.insert("event_caps".into(), Value::Object(event_caps));
    Ok(Value::Object(merged))
}

fn merge_recall_layer(
    merged: &mut serde_json::Map<String, Value>,
    event_caps: &mut serde_json::Map<String, Value>,
    layer: Option<&Value>,
) {
    let Some(layer) = layer.and_then(Value::as_object) else {
        return;
    };
    for (key, value) in layer {
        if key == "event_caps" {
            continue;
        }
        merged.insert(key.clone(), value.clone());
    }
    if let Some(caps) = layer.get("event_caps").and_then(Value::as_object) {
        for (key, value) in caps {
            event_caps.insert(key.clone(), value.clone());
        }
    }
}

/// `memory.write_notice.enabled` for the bound identity (pin `wiring-static.ts:235`).
///
/// Presentation must never depend on config health: an unreadable config keeps the default on.
pub fn resolve_write_notice_enabled(settings: Option<&Value>, identity: Option<&str>) -> bool {
    let Ok(resolved) = resolve_memory_settings(settings) else {
        return true;
    };
    let override_enabled = identity
        .and_then(|identity| resolved["agents"][identity]["write_notice"].get("enabled"))
        .and_then(Value::as_bool);
    override_enabled
        .or_else(|| {
            settings_block(&resolved, "write_notice", default_write_notice_block())
                .get("enabled")
                .and_then(Value::as_bool)
        })
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn agent_overrides_every_field_preserving_trigger_defaults() {
        let settings = serde_json::json!({"reflection":{"enabled":true,"trigger":{"step_count":25,"on_compaction":true},"merge":"auto","category":"quick","timeout_minutes":15,"sandbox":"required"},"agents":{"agent":{"reflection":{"enabled":false,"trigger":{"step_count":5},"merge":"integration","category":"deep","timeout_minutes":30,"sandbox":"off"}}}});
        assert_eq!(resolve_agent_reflection_settings(Some(&settings), "agent").unwrap(), serde_json::json!({"enabled":false,"trigger":{"step_count":5,"on_compaction":true},"merge":"integration","category":"deep","timeout_minutes":30,"sandbox":"off"}));
    }
    #[test]
    fn absent_settings_use_imported_schema_defaults() {
        let defaults = resolve_memory_settings(None).unwrap();
        assert_eq!(defaults["reflection"]["trigger"]["step_count"], 25);
        assert_eq!(resolve_agent_reflection_settings(None, "unknown").unwrap(), defaults["reflection"]);
    }
    #[test]
    fn recall_defaults_apply_when_the_schema_carries_no_recall_block() {
        let resolved = resolve_agent_recall_settings(Some(&serde_json::json!({})), "agent").unwrap();
        assert_eq!(resolved["enabled"], true);
        assert_eq!(resolved["max_items"], 2);
        assert_eq!(resolved["category"], "quick");
        assert_eq!(resolved["event_caps"]["tool_args"], 400);
        assert_eq!(resolved["event_caps"]["result_head"], 600);
        assert_eq!(resolved["event_caps"]["assistant"], 1500);
        assert_eq!(resolved["event_caps"]["prompt"], 4000);
        assert_eq!(resolved["sidecar_max_tokens"], 48000);
        assert_eq!(resolved["max_concurrent_wakes"], 2);
        assert_eq!(resolved["tool_budget"], 8);
        assert_eq!(resolved["query_expansion"], false);
    }
    #[test]
    fn agent_recall_override_wins_and_keeps_the_other_event_caps() {
        let settings = serde_json::json!({"recall":{"enabled":true,"max_items":2,"event_caps":{"tool_args":400,"result_head":600,"assistant":1500,"prompt":4000}},"agents":{"agent":{"recall":{"enabled":false,"max_items":5,"event_caps":{"tool_args":10}}}}});
        let resolved = resolve_agent_recall_settings(Some(&settings), "agent").unwrap();
        assert_eq!(resolved["enabled"], false);
        assert_eq!(resolved["max_items"], 5);
        assert_eq!(resolved["event_caps"]["tool_args"], 10);
        assert_eq!(resolved["event_caps"]["result_head"], 600);
        assert_eq!(resolved["event_caps"]["assistant"], 1500);
        assert_eq!(resolved["event_caps"]["prompt"], 4000);
    }
    #[test]
    fn partial_root_recall_block_keeps_every_pinned_default() {
        let resolved = resolve_agent_recall_settings(Some(&serde_json::json!({"recall":{"enabled":false}})), "agent").unwrap();
        assert_eq!(resolved["enabled"], false);
        assert_eq!(resolved["max_items"], 2);
        assert_eq!(resolved["category"], "quick");
        assert_eq!(resolved["sidecar_max_tokens"], 48000);
        assert_eq!(resolved["max_concurrent_wakes"], 2);
        assert_eq!(resolved["tool_budget"], 8);
        assert_eq!(resolved["query_expansion"], false);
        assert_eq!(resolved["event_caps"]["tool_args"], 400);
        assert_eq!(resolved["event_caps"]["result_head"], 600);
        assert_eq!(resolved["event_caps"]["assistant"], 1500);
        assert_eq!(resolved["event_caps"]["prompt"], 4000);
    }

    #[test]
    fn partial_root_event_caps_merge_per_cap_before_the_agent_layer() {
        let settings = serde_json::json!({"recall":{"event_caps":{"prompt":100}},"agents":{"agent":{"recall":{"event_caps":{"tool_args":10}}}}});
        let resolved = resolve_agent_recall_settings(Some(&settings), "agent").unwrap();
        assert_eq!(resolved["event_caps"]["prompt"], 100);
        assert_eq!(resolved["event_caps"]["tool_args"], 10);
        assert_eq!(resolved["event_caps"]["result_head"], 600);
        assert_eq!(resolved["event_caps"]["assistant"], 1500);
        assert_eq!(resolved["max_items"], 2);
    }
    #[test]
    fn write_notice_defaults_on_and_honours_the_agent_override() {
        assert!(resolve_write_notice_enabled(Some(&serde_json::json!({})), None));
        let base_off = serde_json::json!({"write_notice":{"enabled":false}});
        assert!(!resolve_write_notice_enabled(Some(&base_off), None));
        assert!(!resolve_write_notice_enabled(Some(&base_off), Some("agent")));
        let overridden = serde_json::json!({"write_notice":{"enabled":false},"agents":{"agent":{"write_notice":{"enabled":true}}}});
        assert!(resolve_write_notice_enabled(Some(&overridden), Some("agent")));
        assert!(!resolve_write_notice_enabled(Some(&overridden), Some("other")));
    }
}
