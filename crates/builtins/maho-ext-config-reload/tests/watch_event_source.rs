use maho_ext_config_reload::watch_event_source::subscribe;
use std::{path::PathBuf, sync::{Arc, mpsc}, time::Duration};
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
