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
#[test]
fn equivalent_json_numbers_do_not_turn_a_routine_change_into_a_reload() {
    assert!(is_routine_only_settings_change(Some(r#"{"config":{"limit":[1,0]},"defaultModel":"old"}"#), Some(r#"{"config":{"limit":[1.0,-0.0]},"defaultModel":"new"}"#)));
    assert!(!is_routine_only_settings_change(Some(r#"{"defaultModel":1}"#), Some(r#"{"defaultModel":1.0}"#)));
}
#[test]
fn integer_property_order_matches_javascript_stringify() {
    assert!(is_routine_only_settings_change(Some(r#"{"config":{"2":"b","1":"a"},"defaultModel":"old"}"#), Some(r#"{"config":{"1":"a","2":"b"},"defaultModel":"new"}"#)));
    assert!(!is_routine_only_settings_change(Some(r#"{"config":{"01":"a","4294967295":"b"},"defaultModel":"old"}"#), Some(r#"{"config":{"4294967295":"b","01":"a"},"defaultModel":"new"}"#)));
}
