use serde_json::Value;

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
}
