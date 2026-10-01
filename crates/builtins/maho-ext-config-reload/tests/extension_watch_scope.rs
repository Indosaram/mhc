use maho_ext_config_reload::extension_watch_scope::{is_loadable_extension_entry as loadable, is_scannable_extension_directory as scannable};
use std::{fs, path::Path};

#[test]
fn direct_sources_are_loadable() {
    // Given / When / Then
    assert!(loadable(Path::new("/missing"), "diff.js"));
    assert!(loadable(Path::new("/missing"), "tps.ts"));
}
#[test]
fn direct_non_sources_are_not_loadable() {
    // Given / When / Then
    assert!(!loadable(Path::new("/missing"), "notes.md"));
    assert!(!loadable(Path::new("/missing"), "state.json"));
}
#[test]
fn package_discovery_entries_are_loadable() {
    // Given / When / Then
    for path in ["my-ext/index.ts", "my-ext/index.js", "my-ext/package.json"] { assert!(loadable(Path::new("/missing"), path)); }
}
#[test]
fn undiscovered_helpers_are_not_loadable() {
    // Given / When / Then
    for path in ["my-ext/helper.ts", "my-ext/src/index.ts"] { assert!(!loadable(Path::new("/missing"), path)); }
}
#[test]
fn runtime_state_is_not_watched() {
    // Given / When / Then
    assert!(!loadable(Path::new("/missing"), "goal/no-session/hash/id.json"));
    assert!(!scannable(Path::new("/missing"), "goal/no-session"));
}
#[test]
fn scan_descends_into_root_and_immediate_children() {
    // Given / When / Then
    assert!(scannable(Path::new("/missing"), ""));
    assert!(scannable(Path::new("/missing"), "goal"));
    assert!(!scannable(Path::new("/missing"), "goal/no-session"));
}
fn manifest(content: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap_or_else(|error| panic!("create fixture root: {error}"));
    fs::create_dir(root.path().join("my-ext")).unwrap_or_else(|error| panic!("create fixture package: {error}"));
    fs::write(root.path().join("my-ext/package.json"), content).unwrap_or_else(|error| panic!("write fixture manifest: {error}"));
    root
}
#[test]
fn declared_nested_entry_and_ancestors_stay_watched() {
    // Given
    let root = manifest(r#"{"pi":{"extensions":["dist/index.js"]}}"#);
    // When / Then
    assert!(loadable(root.path(), "my-ext/dist/index.js"));
    assert!(scannable(root.path(), "my-ext/dist"));
    assert!(!loadable(root.path(), "my-ext/dist/state.json"));
}
#[test]
fn absent_manifest_keeps_runtime_state_unwatched() {
    // Given
    let root = tempfile::tempdir().unwrap();
    // When / Then
    assert!(!scannable(root.path(), "goal/no-session"));
    assert!(!loadable(root.path(), "goal/no-session/hash/id.json"));
}
#[test]
fn dot_prefixed_manifest_entries_are_resolved() {
    // Given
    let root = manifest(r#"{"pi":{"extensions":["./ext1.ts","./ext2.ts"]}}"#);
    // When / Then
    assert!(loadable(root.path(), "my-ext/ext1.ts"));
    assert!(loadable(root.path(), "my-ext/ext2.ts"));
    assert!(!loadable(root.path(), "my-ext/ext3.ts"));
}
#[test]
fn manifest_entry_escaping_extension_directory_is_ignored() {
    // Given
    let root = manifest(r#"{"pi":{"extensions":["../../outside.js"]}}"#);
    // When / Then
    assert!(!loadable(root.path(), "my-ext/../../outside.js"));
}
#[test]
fn malformed_manifest_does_not_throw() {
    // Given
    let root = manifest("{ not json");
    // When / Then
    assert!(!loadable(root.path(), "my-ext/dist/index.js"));
    assert!(loadable(root.path(), "my-ext/package.json"));
}
