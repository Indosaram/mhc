use maho_ext_config_reload::watch_engine::*;
use std::{sync::Arc, time::Duration};
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
