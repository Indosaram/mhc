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
    let groups = group_changed_paths(&paths, &targets);
    assert!(validate_builtin_paths(&groups["builtin"], root.path(), root.path()).is_empty());
    let mut pending = PendingChanges::default();
    pending.add("builtin", &groups["builtin"]);
    assert_eq!(pending.snapshot()[0].paths, [path]);
    assert_eq!(reload_admission(pending.is_empty(), false, true, false, false, true), ReloadAdmission::ProbeVeto);
    engine.close().unwrap();
}
