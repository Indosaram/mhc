use maho_ext_config_reload::index::*;
use serde_json::json;
#[test]
fn pending_paths_deduplicate_sort_and_remove_per_registration() {
    let mut pending = PendingChanges::default();
    pending.add("first", &["b".into(), "a".into(), "b".into()]);
    pending.add("second", &["c".into()]);
    assert_eq!(pending.snapshot()[0].paths, vec![std::path::PathBuf::from("a"), "b".into()]);
    pending.delete("first");
    assert_eq!(pending.snapshot().len(), 1);
    pending.clear();
    assert!(pending.is_empty());
}
#[test]
fn handoff_snapshot_diff_covers_changed_created_and_deleted_paths() {
    let previous = std::collections::BTreeMap::from([("same".into(), "hash".into()), ("changed".into(), "old".into()), ("deleted".into(), "hash".into())]);
    let next = std::collections::BTreeMap::from([("same".into(), "hash".into()), ("changed".into(), "new".into()), ("created".into(), "hash".into())]);
    assert_eq!(compare_snapshots(&previous, &next), vec![std::path::PathBuf::from("changed"), "created".into(), "deleted".into()]);
}
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
#[test]
fn keybindings_validation_rejects_non_string_values_and_allows_deletion() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("keybindings.json");
    std::fs::write(&path, "{\"fixture\":1}").unwrap();
    assert_eq!(validate_builtin_paths(std::slice::from_ref(&path), root.path(), root.path()).len(), 1);
    std::fs::write(&path, "{\"fixture\":[\"ctrl+x\"]}").unwrap();
    assert!(validate_builtin_paths(std::slice::from_ref(&path), root.path(), root.path()).is_empty());
    std::fs::remove_file(&path).unwrap();
    assert!(validate_builtin_paths(&[path], root.path(), root.path()).is_empty());
}
#[test]
fn routine_suppression_advances_snapshot_before_registration_grouping() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("settings.json");
    let mut contents = std::collections::BTreeMap::from([(path.clone(), "{\"defaultModel\":\"old\"}".into())]);
    let mut logger = maho_ext_config_reload::log::ConfigReloadLogger::new(root.path(), None).unwrap();
    std::fs::write(&path, "{\"defaultModel\":\"new\"}").unwrap();
    assert!(significant_changed_paths(std::slice::from_ref(&path), &Default::default(), &mut contents, root.path(), root.path(), &mut logger).is_empty());
    std::fs::write(&path, "{\"defaultModel\":\"new\",\"theme\":\"dark\"}").unwrap();
    assert_eq!(significant_changed_paths(std::slice::from_ref(&path), &Default::default(), &mut contents, root.path(), root.path(), &mut logger), [path]);
}
