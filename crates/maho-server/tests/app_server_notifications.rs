use maho_server::app_server::notifications::*;
use serde_json::json;
use std::{collections::BTreeSet, sync::{Arc, atomic::{AtomicBool, Ordering}}};

#[tokio::test]
async fn retained_terminal_notification_replays_with_original_timestamp_and_disconnect_cleans_subscriptions() {
    let mut router = NotificationRouter::default();
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    router.add_thread("thread".into(), RoutableThread::default());
    router.to_thread("thread", json!({"method":"turn/completed","params":{"turnId":"one"}}), 42);
    router.add_connection(RoutableConnection { id: "client".into(), initialized: true, stdio: false,
        experimental_api: false, opt_out_notification_methods: BTreeSet::new(), close: None,
        send: Arc::new(move |message| { send.send(message).unwrap(); Box::pin(async { Ok(()) }) }) });
    router.subscribe("thread", "client");
    let message = tokio::time::timeout(std::time::Duration::from_secs(2), receive.recv()).await.unwrap().unwrap();
    assert_eq!(message["emittedAtMs"], 42);
    assert_eq!(message["params"]["turnId"], "one");
    assert_eq!(router.remove_connection("client"), vec!["thread"]);
    assert!(router.remove_connection("client").is_empty());
}

#[tokio::test]
async fn queue_cap_closes_non_stdio_client_and_requests_bypass_notification_filters() {
    let mut router = NotificationRouter::default();
    router.outbound_queue_limit = 1;
    let closed = Arc::new(AtomicBool::new(false));
    let close_flag = closed.clone();
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let (_release, blocked) = tokio::sync::watch::channel(false);
    router.add_connection(RoutableConnection { id: "client".into(), initialized: true, stdio: false,
        experimental_api: false, opt_out_notification_methods: BTreeSet::from(["turn/completed".into()]),
        close: Some(Arc::new(move || { close_flag.store(true, Ordering::SeqCst); })),
        send: Arc::new(move |message| { send.send(message).unwrap(); let mut blocked = blocked.clone();
            Box::pin(async move { let _result = blocked.wait_for(|value| *value).await; Ok(()) }) }) });
    router.add_thread("thread".into(), RoutableThread::default());
    router.subscribe("thread", "client");
    router.to_thread("thread", json!({"method":"turn/completed"}), 1);
    assert!(receive.try_recv().is_err());
    router.to_thread("thread", json!({"id":1,"method":"turn/completed"}), 2);
    assert_eq!(receive.try_recv().unwrap()["id"], 1);
    router.to_thread("thread", json!({"id":2,"method":"turn/completed"}), 3);
    assert!(closed.load(Ordering::SeqCst));
    assert!(receive.try_recv().is_err());
    assert!(router.broadcast(json!({"method":"turn/completed"}), 4).is_err());
}
