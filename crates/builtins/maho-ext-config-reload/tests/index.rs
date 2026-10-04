use maho_ext_config_reload::index::*;
use serde_json::json;
#[test]
fn registration_rejection_suppression_and_identity_do_not_clear_pending() {
    use maho_ext_config_reload::protocol::*;
    use std::{path::Path, sync::Arc};
    let root = Path::new("/fixture");
    let registration = Arc::new(ConfigWatchRegistration { id: "r".into(), display_name: "fixture".into(), targets: vec![ConfigWatchTarget { path: "auth.json".into(), kind: ConfigWatchTargetKind::File, filter_globs: None }] });
    let mut registrations = WatchRegistrations::default();
    let mut pending = PendingChanges::default();
    assert_eq!(registrations.register(Arc::clone(&registration), root, root, &mut pending), RegistrationAdmission::Restricted);
    assert_eq!(registrations.register(registration, root, root, &mut pending), RegistrationAdmission::RejectionSuppressed);
    let safe = Arc::new(ConfigWatchRegistration { id: "r".into(), display_name: "fixture".into(), targets: vec![] });
    assert_eq!(registrations.register(Arc::clone(&safe), root, root, &mut pending), RegistrationAdmission::Added);
    pending.add("r", &["settings.json".into()]);
    assert_eq!(registrations.register(Arc::clone(&safe), root, root, &mut pending), RegistrationAdmission::Identical);
    assert!(!pending.is_empty());
    assert_eq!(registrations.register(Arc::new((*safe).clone()), root, root, &mut pending), RegistrationAdmission::Added);
    assert!(pending.is_empty());
    assert!(registrations.unregister("r", &mut pending));
    assert!(!registrations.unregister("r", &mut pending));
}
#[test]
fn reload_admission_probes_veto_only_after_idle_compaction_and_capability_gates() {
    assert_eq!(reload_admission(true, false, true, false, false, true), ReloadAdmission::Empty);
    assert_eq!(reload_admission(false, true, true, false, false, true), ReloadAdmission::InFlight);
    assert_eq!(reload_admission(false, false, false, false, true, true), ReloadAdmission::Busy);
    assert_eq!(reload_admission(false, false, true, true, false, true), ReloadAdmission::Busy);
    assert_eq!(reload_admission(false, false, true, false, true, false), ReloadAdmission::Compacting);
    assert_eq!(reload_admission(false, false, true, false, false, false), ReloadAdmission::Unavailable);
    assert_eq!(reload_admission(false, false, true, false, false, true), ReloadAdmission::ProbeVeto);
}
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
fn pending_registration_order_follows_first_observation_not_name() {
    let mut pending = PendingChanges::default();
    pending.add("z", &["first".into()]);
    pending.add("a", &["second".into()]);
    pending.add("z", &["third".into()]);
    assert_eq!(pending.snapshot().iter().map(|change| change.registration_id.as_str()).collect::<Vec<_>>(), ["z", "a"]);
    pending.delete("z");
    pending.add("z", &["new".into()]);
    assert_eq!(pending.snapshot().iter().map(|change| change.registration_id.as_str()).collect::<Vec<_>>(), ["a", "z"]);
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
