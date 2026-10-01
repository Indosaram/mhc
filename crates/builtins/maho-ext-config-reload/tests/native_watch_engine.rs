use maho_ext_config_reload::watch_engine::*;
use std::{sync::Arc, time::Duration};
#[tokio::test]
async fn async_native_events_use_signal_without_polling() {
    let root = tempfile::tempdir().unwrap();
    let mut engine = NativeWatchEngine::with_source(vec![WatchTarget { id: "fixture".into(), kind: WatchKind::DirRecursive, path: root.path().into(), allow_list: None, filter: None }], Arc::new(|error, _| panic!("{error}")), Duration::ZERO, maho_ext_config_reload::watch_event_source::FsWatchEventSource::default()).unwrap();
    let path = root.path().join("settings.json");
    let staged = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(staged.path(), "{}").unwrap();
    std::fs::rename(staged.path(), &path).unwrap();
    let change = tokio::time::timeout(Duration::from_secs(5), engine.next_change_async()).await.unwrap().unwrap();
    assert_eq!(change.created, [path]);
    engine.close_async().await.unwrap();
    engine.close_async().await.unwrap();
}
#[test]
fn native_events_drive_hash_gated_changes() {
    let root = tempfile::tempdir().unwrap();
    let mut engine = NativeWatchEngine::with_debounce(vec![WatchTarget { id: "fixture".into(), kind: WatchKind::DirRecursive, path: root.path().into(), allow_list: None, filter: None }], Arc::new(|error, _| panic!("{error}")), Duration::ZERO).unwrap();
    let file = root.path().join("settings.json");
    let staged = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(staged.path(), "{}").unwrap();
    std::fs::rename(staged.path(), &file).unwrap();
    let change = engine.next_change(Duration::from_secs(5)).unwrap();
    assert_eq!(change.created, [file]);
    engine.close().unwrap();
}
#[test]
fn native_config_change_reaches_validation_and_pending_registration() {
    use maho_ext_config_reload::{index::*, log::ConfigReloadLogger, routine_settings::refresh_settings_content_snapshots};
    let root = tempfile::tempdir().unwrap();
    let settings = resolve_config_reload_settings(&serde_json::json!({}), &serde_json::json!({}));
    let targets = build_builtin_watch_targets(root.path(), root.path(), false, &settings, &[]);
    let watched = build_builtin_watch_targets(root.path(), root.path(), false, &settings, &[]).into_iter().map(|active| active.target).collect();
    let mut engine = NativeWatchEngine::with_debounce(watched, Arc::new(|error, _| panic!("{error}")), Duration::ZERO).unwrap();
    let mut contents = Default::default();
    refresh_settings_content_snapshots(&mut contents, root.path(), root.path());
    let path = root.path().join("keybindings.json");
    let staged = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(staged.path(), "{\"fixture\":\"ctrl+x\"}").unwrap();
    std::fs::rename(staged.path(), &path).unwrap();
    let change = engine.next_change(Duration::from_secs(5)).unwrap();
    let mut logger = ConfigReloadLogger::new(root.path(), None).unwrap();
    let paths = significant_changed_paths(&change.changed_paths, &engine.engine.get_baseline_snapshot(), &mut contents, root.path(), root.path(), &mut logger);
    let groups = group_changed_paths(&paths, &targets);
    assert!(validate_builtin_paths(&groups["builtin"], root.path(), root.path()).is_empty());
    let mut pending = PendingChanges::default();
    pending.add("builtin", &groups["builtin"]);
    assert_eq!(pending.snapshot()[0].paths, [path]);
    assert_eq!(reload_admission(pending.is_empty(), false, true, false, false, true), ReloadAdmission::ProbeVeto);
    engine.close().unwrap();
}
