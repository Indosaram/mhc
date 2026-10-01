use maho_ext_config_reload::index::*;
use serde_json::json;
#[test]
fn handoffs_are_session_keyed_and_consumed_once() {
    let mut registry = ConfigReloadHandoffRegistry::default();
    registry.set("first".into(), 1);
    registry.set("second".into(), 2);
    assert_eq!(registry.take("first"), Some(1));
    assert_eq!(registry.take("first"), None);
    registry.delete("second");
    assert_eq!(registry.take("second"), None);
}
#[test]
fn project_settings_override_only_valid_fields() {
    let global = json!({"configReload":{"enabled":false,"debounceMs":12.5,"watch":{"settings":false,"models":false}}});
    let project = json!({"configReload":{"enabled":true,"debounceMs":-1,"watch":{"settings":true,"models":"invalid"}}});
    let settings = resolve_config_reload_settings(&global, &project);
    assert!(settings.enabled);
    assert_eq!(settings.debounce_ms, 12.0);
    assert!(settings.watch["settings"]);
    assert!(!settings.watch["models"]);
    assert!(settings.watch["skills"]);
}
