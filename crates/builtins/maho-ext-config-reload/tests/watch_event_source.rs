use maho_ext_config_reload::watch_event_source::subscribe;
use std::{path::PathBuf, sync::{Arc, mpsc}, time::Duration};
#[test]
fn worker_dispatch_crash_rebuilds_surviving_watchers() {
    use maho_ext_config_reload::watch_event_source::{FsWatchEventSource, NativeEventCallback};
    use std::sync::{Mutex, atomic::{AtomicBool, Ordering}};
    // Given
    let root = tempfile::tempdir().unwrap();
    let callbacks: Arc<Mutex<Vec<NativeEventCallback>>> = Arc::default();
    let listeners = Arc::clone(&callbacks);
    let (rebuilt, replacements) = mpsc::channel();
    let source = FsWatchEventSource::with_factory(Arc::new(move |_, _, callback| {
        listeners.lock().unwrap().push(callback);
        rebuilt.send(()).unwrap();
        Ok(Box::new(()))
    }));
    let fail = Arc::new(AtomicBool::new(true));
    let (delivered, received) = mpsc::channel();
    let mut subscription = source.subscribe(root.path().into(), false, Arc::new(move |_, _| {
        assert!(!fail.swap(false, Ordering::SeqCst), "fixture dispatch crash");
        delivered.send(()).unwrap();
    }), Arc::new(|_, _| {})).unwrap();
    subscription.ready().unwrap();
    replacements.recv_timeout(Duration::from_secs(5)).unwrap();
    // When
    callbacks.lock().unwrap()[0](Ok(notify::Event::new(notify::EventKind::Any)));
    replacements.recv_timeout(Duration::from_secs(5)).unwrap();
    callbacks.lock().unwrap()[1](Ok(notify::Event::new(notify::EventKind::Any)));
    // Then
    received.recv_timeout(Duration::from_secs(5)).unwrap();
    subscription.close().unwrap();
}

#[tokio::test]
async fn repeated_subscription_close_waits_for_original_join() {
    // Given
    let root = tempfile::tempdir().unwrap();
    let source = maho_ext_config_reload::watch_event_source::FsWatchEventSource::default();
    let mut subscription = source.subscribe(root.path().into(), false, Arc::new(|_, _| {}), Arc::new(|error, _| panic!("{error}"))).unwrap();
    subscription.ready().unwrap();
    let first = subscription.close_async();
    // When
    tokio::time::timeout(Duration::from_secs(5), subscription.close_async()).await.unwrap().unwrap();
    // Then
    use futures::FutureExt;
    assert_eq!(first.now_or_never(), Some(Ok(())));
}

#[tokio::test]
async fn async_close_cancels_synchronously_then_joins_worker() {
    let root = tempfile::tempdir().unwrap();
    let source = maho_ext_config_reload::watch_event_source::FsWatchEventSource::default();
    let mut subscription = source.subscribe(root.path().into(), false, Arc::new(|_, _| {}), Arc::new(|error, _| panic!("{error}"))).unwrap();
    subscription.ready().unwrap();
    let closing = subscription.close_async();
    subscription.close().unwrap();
    closing.await.unwrap();
    subscription.close_async().await.unwrap();
}
#[test]
fn isolated_sources_keep_other_registry_alive_after_close() {
    use maho_ext_config_reload::watch_event_source::FsWatchEventSource;
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let source = FsWatchEventSource::default();
    let independent = FsWatchEventSource::default();
    let (sender, receiver) = mpsc::channel();
    let mut one = source.subscribe(first.path().into(), false, Arc::new(|_, _| {}), Arc::new(|error, _| panic!("{error}"))).unwrap();
    let mut two = independent.subscribe(second.path().into(), false, Arc::new(move |_, filename| { if filename == Some("settings.json".into()) { sender.send(()).unwrap(); } }), Arc::new(|error, _| panic!("{error}"))).unwrap();
    one.ready().unwrap();
    two.ready().unwrap();
    one.close().unwrap();
    std::fs::write(second.path().join("settings.json"), "{}").unwrap();
    receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    two.close().unwrap();
}
#[test]
fn native_watch_delivers_creation_and_joins_teardown() {
    let root = tempfile::tempdir().unwrap();
    let (sender, receiver) = mpsc::channel();
    let error_sender = sender.clone();
    let mut subscription = subscribe(root.path().into(), false, Arc::new(move |_, filename| {
        if filename == Some(PathBuf::from("settings.json")) { sender.send(Ok(())).unwrap(); }
    }), Arc::new(move |error, _| { error_sender.send(Err(error)).unwrap(); })).unwrap();
    subscription.ready().unwrap();
    std::fs::write(root.path().join("settings.json"), "{}").unwrap();
    receiver.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    subscription.close().unwrap();
    subscription.close().unwrap();
}
