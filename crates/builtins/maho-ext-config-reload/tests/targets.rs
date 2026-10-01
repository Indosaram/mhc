use maho_ext_config_reload::index::{build_builtin_watch_targets, resolve_config_reload_settings};
use serde_json::json;
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
