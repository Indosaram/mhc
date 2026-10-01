use maho_ext_config_reload::watch_engine::*;
use std::fs;
fn target(path: &std::path::Path) -> WatchTarget { WatchTarget { id: "fixture".into(), kind: WatchKind::DirRecursive, path: path.into(), allow_list: None, filter: None } }
#[test]
fn hash_gate_tracks_creation_edit_deletion_and_close() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("settings.json");
    let mut engine = ConfigReloadWatchEngine::new(vec![target(root.path())]).unwrap();
    fs::write(&file, "one").unwrap();
    assert_eq!(engine.evaluate().unwrap().created, std::slice::from_ref(&file));
    assert!(engine.evaluate().unwrap().changed_paths.is_empty());
    fs::write(&file, "two").unwrap();
    let edit = engine.evaluate().unwrap();
    assert_eq!(edit.changed_paths, std::slice::from_ref(&file));
    assert!(edit.created.is_empty());
    fs::remove_file(&file).unwrap();
    assert_eq!(engine.evaluate().unwrap().deleted, std::slice::from_ref(&file));
    engine.close();
    fs::write(file, "three").unwrap();
    assert!(engine.evaluate().unwrap().changed_paths.is_empty());
}
#[test]
fn scan_omits_symlinks_dependencies_and_unallowed_dot_directories() {
    let root = tempfile::tempdir().unwrap();
    for dir in ["node_modules", ".git", ".hidden", "nested"] { fs::create_dir(root.path().join(dir)).unwrap(); fs::write(root.path().join(dir).join("file"), "fixture").unwrap(); }
    std::os::unix::fs::symlink(root.path().join("nested"), root.path().join("alias")).unwrap();
    let engine = ConfigReloadWatchEngine::new(vec![target(root.path())]).unwrap();
    assert_eq!(engine.get_baseline_snapshot().len(), 1);
    assert_eq!(engine.watched_directories().len(), 2);
}
#[test]
fn scan_reports_bad_path_and_continues_other_targets() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("file");
    fs::write(&file, "fixture").unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let invalid = file.join("child");
    let engine = ConfigReloadWatchEngine::with_error_listener(vec![target(&invalid), target(root.path())], std::sync::Arc::new(move |error, path| { sender.send((error, path)).unwrap(); })).unwrap();
    assert_eq!(receiver.try_recv().unwrap().1, invalid);
    assert_eq!(engine.get_baseline_snapshot().len(), 1);
}
#[test]
fn affected_scan_preserves_unobserved_siblings_until_their_event() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    fs::write(&first, "old").unwrap();
    fs::write(&second, "old").unwrap();
    let mut engine = ConfigReloadWatchEngine::new(vec![target(root.path())]).unwrap();
    fs::write(&first, "new").unwrap();
    fs::write(&second, "new").unwrap();
    assert_eq!(engine.evaluate_affected(&std::collections::BTreeSet::from([first.clone()])).unwrap().changed_paths, [first]);
    assert_eq!(engine.evaluate().unwrap().changed_paths, [second]);
}
#[test]
fn watch_roots_normalize_dot_segments_before_snapshot_keys() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("nested")).unwrap();
    let file = root.path().join("file");
    fs::write(&file, "fixture").unwrap();
    let engine = ConfigReloadWatchEngine::new(vec![target(&root.path().join("nested/.."))]).unwrap();
    assert!(engine.get_baseline_snapshot().contains_key(&file));
    assert!(engine.watched_directories().contains(root.path()));
}
#[test]
fn injected_hash_scans_only_the_observed_file() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    fs::write(&first, "fixture").unwrap();
    fs::write(root.path().join("second"), "fixture").unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut engine = ConfigReloadWatchEngine::with_hash_file(vec![target(root.path())], None, std::sync::Arc::new(move |path| { sender.send(path.to_path_buf()).unwrap(); Ok("hash".into()) })).unwrap();
    assert_eq!(receiver.try_iter().count(), 2);
    assert!(engine.evaluate_affected(&std::collections::BTreeSet::from([first.clone()])).unwrap().changed_paths.is_empty());
    assert_eq!(receiver.try_iter().collect::<Vec<_>>(), [first]);
}
