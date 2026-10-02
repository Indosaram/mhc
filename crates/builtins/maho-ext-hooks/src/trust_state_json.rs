use serde_json::Value;
use crate::types::{HookTrustEntry, HookTrustState};

pub fn empty_hook_trust_state() -> HookTrustState {
    HookTrustState { version: 1, hooks: Default::default() }
}

pub fn parse_hook_trust_state_json(input: Option<&str>) -> Option<HookTrustState> {
    let input = input.filter(|value| !value.trim().is_empty())?;
    let value: Value = serde_json::from_str(input).ok()?;
    let value = value.as_object()?;
    if value.get("version").and_then(Value::as_f64) != Some(1.0) { return None; }
    let hooks = value.get("hooks")?.as_object()?;
    let mut state = empty_hook_trust_state();
    for (id, entry) in hooks {
        if !entry.is_object() { continue; }
        if ["trustedHash", "matcher"].iter().any(|field| entry.get(field).is_some_and(|value| !value.is_string())) { continue; }
        if let Ok(entry) = serde_json::from_value::<HookTrustEntry>(entry.clone()) { state.hooks.insert(id.clone(),entry); }
    }
    Some(state)
}

pub fn read_hook_trust_state_json(input: Option<&str>) -> HookTrustState {
    parse_hook_trust_state_json(input).unwrap_or_else(empty_hook_trust_state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn invalid_root_defaults_without_trusting_entries() {
        for input in [None, Some(""), Some("  "), Some("{"), Some("[]"), Some(r#"{"version":2,"hooks":{}}"#), Some(r#"{"version":1,"hooks":[]}"#)] {
            assert!(parse_hook_trust_state_json(input).is_none());
            assert_eq!(read_hook_trust_state_json(input),empty_hook_trust_state());
        }
    }

    #[test]
    fn valid_entries_are_normalized_invalid_entries_dropped() {
        let valid = json!({"enabled":true,"scope":"project","sourcePath":"/repo/hooks.json","commandPreview":"node hook","updatedAt":"fixed","extra":"discard"});
        let mut hooks = serde_json::Map::new(); hooks.insert("valid".to_owned(),valid.clone());
        for (field,value) in [("enabled",Value::Null),("trustedHash",Value::Null),("scope",json!("unknown")),("sourcePath",json!(3)),("matcher",Value::Null),("commandPreview",Value::Null),("updatedAt",Value::Null)] {
            let mut invalid = valid.clone(); invalid[field] = value; hooks.insert(field.to_owned(),invalid);
        }
        let input = json!({"version":1,"hooks":hooks}).to_string();
        let state = read_hook_trust_state_json(Some(&input)); assert_eq!(state.hooks.len(),1);
        let output = serde_json::to_value(&state).expect("serialize state");
        assert_eq!(output["hooks"]["valid"],json!({"enabled":true,"scope":"project","sourcePath":"/repo/hooks.json","commandPreview":"node hook","updatedAt":"fixed"}));
    }
}
