use maho_ext_config_reload::index::{build_builtin_watch_targets, resolve_config_reload_settings};
use serde_json::json;
use maho_ext_config_reload::{index::{build_external_watch_targets, group_changed_paths}, protocol::*};
#[test]
fn external_file_filters_and_literal_dot_directory_are_grouped() {
    let root = tempfile::tempdir().unwrap();
    let registrations = vec![ConfigWatchRegistration { id: "external".into(), display_name: "fixture".into(), targets: vec![ConfigWatchTarget { path: "./settings.json".into(), kind: ConfigWatchTargetKind::File, filter_globs: None }, ConfigWatchTarget { path: ".".into(), kind: ConfigWatchTargetKind::Dir, filter_globs: Some(vec!["/.omo".into()]) }] }];
    let targets = build_external_watch_targets(root.path(), &registrations);
    assert_eq!(targets[1].target.allow_list.as_ref().unwrap(), &[std::path::PathBuf::from(".omo")]);
    let paths = vec![root.path().join("settings.json"), root.path().join(".omo"), root.path().join("unmatched")];
    let groups = group_changed_paths(&paths, &targets);
    assert_eq!(groups["external"], paths[..2]);
    assert_eq!(groups["builtin"], paths[2..]);
}
#[test]
fn missing_resources_use_parent_presence_and_untrusted_project_is_omitted() {
    let root = tempfile::tempdir().unwrap();
    let agent = root.path().join("agent");
    std::fs::create_dir(&agent).unwrap();
    let settings = resolve_config_reload_settings(&json!({}), &json!({}));
    let targets = build_builtin_watch_targets(root.path(), &agent, false, &settings, &[]);
    assert_eq!(targets.len(), 3);
    assert!(targets.iter().all(|target| !target.target.id.contains("project")));
    assert_eq!(targets.iter().filter(|target| target.rearm_on_creation.is_some()).count(), 2);
}
#[test]
fn trusted_project_presence_watches_config_name() {
    let root = tempfile::tempdir().unwrap();
    let settings = resolve_config_reload_settings(&json!({}), &json!({}));
    let targets = build_builtin_watch_targets(root.path(), root.path(), true, &settings, &[]);
    let presence = targets.iter().find(|target| target.target.id == "builtin-project-presence").unwrap();
    assert!(presence.rearm_on_creation.is_some());
    assert_eq!(presence.target.allow_list.as_ref().unwrap().len(), 1);
}
