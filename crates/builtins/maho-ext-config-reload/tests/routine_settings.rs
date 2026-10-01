use std::{collections::BTreeMap, fs};
use maho_ext_config_reload::{log::ConfigReloadLogger, routine_settings::{is_routine_only_settings_change, refresh_settings_content_snapshots, exclude_routine_only_settings_changes}};

#[test]
fn only_routine_keys_are_suppressed() {
    assert!(is_routine_only_settings_change(Some(r#"{"theme":"dark","defaultModel":"m1"}"#), Some(r#"{"theme":"dark","defaultModel":"m2"}"#)));
    assert!(!is_routine_only_settings_change(Some(r#"{"theme":"dark"}"#), Some(r#"{"theme":"light","defaultModel":"m2"}"#)));
}
#[test]
fn missing_invalid_and_unchanged_settings_are_not_suppressed() {
    assert!(!is_routine_only_settings_change(None, Some("{}")));
    assert!(!is_routine_only_settings_change(Some("{}"), Some("{nope")));
    assert!(!is_routine_only_settings_change(Some("{}"), Some("{}")));
}
#[test]
fn snapshots_advance_after_suppressed_change() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("settings.json");
    fs::write(&path, r#"{"theme":"dark","defaultModel":"m1"}"#).unwrap();
    let mut snapshots = BTreeMap::new();
    refresh_settings_content_snapshots(&mut snapshots, root.path(), root.path());
    let mut logger = ConfigReloadLogger::new(root.path(), None).unwrap();
    fs::write(&path, r#"{"theme":"dark","defaultModel":"m2"}"#).unwrap();
    assert!(exclude_routine_only_settings_changes(std::slice::from_ref(&path), &mut snapshots, root.path(), root.path(), &mut logger).is_empty());
    fs::write(&path, r#"{"theme":"light","defaultModel":"m3"}"#).unwrap();
    assert_eq!(exclude_routine_only_settings_changes(std::slice::from_ref(&path), &mut snapshots, root.path(), root.path(), &mut logger), [path]);
}
#[test]
fn jsonc_routine_changes_use_shared_parser() {
    assert!(is_routine_only_settings_change(Some("{/* before */\"defaultModel\":\"m1\",}"), Some("{\"defaultModel\":\"m2\"}")));
}
