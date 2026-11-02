use maho_ext_config_reload::watch_engine::*;
use std::{sync::Arc, time::Duration};
#[tokio::test]
async fn injected_clock_and_event_source_gate_debounce_without_wall_time() {
    use maho_ext_config_reload::watch_event_source::{FsWatchEventSource, NativeEventCallback};
    use std::sync::Mutex;
    struct Clock { armed: tokio::sync::mpsc::UnboundedSender<tokio::time::Instant>, release: Arc<tokio::sync::Notify> }
    impl WatchClock for Clock {
        fn now(&self) -> tokio::time::Instant { tokio::time::Instant::now() }
        fn sleep_until(&self, deadline: tokio::time::Instant) -> futures::future::BoxFuture<'static, ()> {
            assert!(self.armed.send(deadline).is_ok());
            let release = Arc::clone(&self.release);
            Box::pin(async move { release.notified().await })
        }
    }
    // Given
    let root = tempfile::tempdir().unwrap();
    let callback: Arc<Mutex<Option<NativeEventCallback>>> = Arc::default();
    let registered = Arc::clone(&callback);
    let source = FsWatchEventSource::with_factory(Arc::new(move |_, _, listener| {
        *registered.lock().unwrap() = Some(listener);
        Ok(Box::new(()))
    }));
    let mut engine = NativeWatchEngine::with_source(vec![WatchTarget { id: "fixture".into(), kind: WatchKind::Dir, path: root.path().into(), allow_list: None, filter: None }], Arc::new(|error, _| panic!("{error}")), Duration::from_secs(100), source).unwrap();
    let (armed, mut timer) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    engine.set_clock(Arc::new(Clock { armed, release: Arc::clone(&release) }));
    let path = root.path().join("settings.json");
    std::fs::write(&path, "{}").unwrap();
    {
        let mut callback = callback.lock().expect("watch callback mutex must remain unpoisoned");
        let listener = callback.as_mut().expect("watch factory must register the callback");
        listener(Ok(notify::Event::new(notify::EventKind::Any).add_path(path.clone())));
    }
    // When
    let change = engine.next_change_async();
    tokio::pin!(change);
    tokio::select! {
        result = &mut change => panic!("debounce returned before clock release: {}", result.is_ok()),
        timer = timer.recv() => assert!(timer.is_some()),
    }
    release.notify_one();
    let actual = tokio::time::timeout(Duration::from_secs(5), &mut change).await.unwrap().unwrap();
    // Then
    assert_eq!(actual.created, [path]);
}

#[tokio::test]
async fn repeated_close_joins_the_original_pending_disposal() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let mut engine = NativeWatchEngine::with_source(vec![WatchTarget { id: "fixture".into(), kind: WatchKind::Dir, path: root.path().into(), allow_list: None, filter: None }], Arc::new(|error, _| panic!("{error}")), Duration::ZERO, maho_ext_config_reload::watch_event_source::FsWatchEventSource::default()).unwrap();
    let first = engine.close_async();
    // When
    let repeated = engine.close_async();
    tokio::time::timeout(Duration::from_secs(5), repeated).await.unwrap().unwrap();
    // Then
    use futures::FutureExt;
    assert_eq!(first.now_or_never(), Some(Ok(())));
}

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
    let groups = group_changed_paths(&paths, &targets, &[]);
    assert!(validate_builtin_paths(&groups["builtin"], root.path(), root.path()).is_empty());
    let mut pending = PendingChanges::default();
    pending.add("builtin", &groups["builtin"]);
    assert_eq!(pending.snapshot()[0].paths, [path]);
    assert_eq!(reload_admission(pending.is_empty(), false, true, false, false, true), ReloadAdmission::ProbeVeto);
    engine.close().unwrap();
}

/// A watched `.omo` and the config file inside it can arrive in separate batches: the directory's
/// event alone is a bare directory change, and the file may be written after the directory was
/// scanned but before its own watch exists, so no event ever names it. The engine must still report
/// that file. The fake watcher writes the file at the moment the directory's watch is created, the
/// latest point the race allows, and never raises an event for it.
#[tokio::test]
async fn file_written_before_a_new_directory_watch_is_still_reported() {
    use maho_ext_config_reload::watch_event_source::{FsWatchEventSource, NativeEventCallback};
    use std::{collections::BTreeMap, path::PathBuf, sync::Mutex};
    let root = tempfile::tempdir().unwrap();
    let omo = root.path().join(".omo");
    let config = omo.join("omo.jsonc");
    let callbacks: Arc<Mutex<BTreeMap<PathBuf, NativeEventCallback>>> = Arc::default();
    let registered = Arc::clone(&callbacks);
    let late_config = config.clone();
    let source = FsWatchEventSource::with_factory(Arc::new(move |path, _, listener| {
        if path.file_name().is_some_and(|name| name == ".omo") { std::fs::write(&late_config, "{}")?; }
        registered.lock().unwrap().insert(path.to_path_buf(), listener);
        Ok(Box::new(()))
    }));
    let target = WatchTarget { id: "fixture".into(), kind: WatchKind::DirRecursive, path: root.path().into(), allow_list: Some(vec![".omo".into()]), filter: None };
    let mut engine = NativeWatchEngine::with_source(vec![target], Arc::new(|error, _| panic!("{error}")), Duration::ZERO, source).unwrap();
    std::fs::create_dir(&omo).unwrap();
    {
        let mut callbacks = callbacks.lock().unwrap();
        let listener = callbacks.get_mut(root.path()).expect("root watch registered");
        listener(Ok(notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::Folder)).add_path(omo.clone())));
    }
    let directory = tokio::time::timeout(Duration::from_secs(5), engine.next_change_async()).await.unwrap().unwrap();
    assert_eq!(directory.changed_paths, std::slice::from_ref(&omo));
    assert_eq!(directory.directories, std::slice::from_ref(&omo));
    let file = tokio::time::timeout(Duration::from_secs(5), engine.next_change_async()).await.unwrap().unwrap();
    assert_eq!(file.created, [config]);
    assert!(file.directories.is_empty());
    engine.close_async().await.unwrap();
}
