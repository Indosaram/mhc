use maho_server::app_server::{fuzzy_files::FuzzyFileEntry, fuzzy_search_service::FuzzyFileSearchService, fuzzy_search_methods::register_fuzzy_file_search_methods, registry::{MethodRegistry, RegistryConnection}};
use serde_json::json;
use std::{sync::{Arc, Mutex, atomic::Ordering}, time::Duration};

#[tokio::test]
async fn sessions_emit_latest_query_in_order_and_reject_after_stop() {
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let (ready, signal) = tokio::sync::oneshot::channel();
    let signal = Arc::new(Mutex::new(Some(signal)));
    let service = FuzzyFileSearchService::with_collector(Arc::new(move |message| { send.send(message).unwrap(); }), Arc::new(move |_, _| {
        let signal = signal.lock().unwrap().take().unwrap();
        Box::pin(async move {
            signal.await.unwrap();
            vec![FuzzyFileEntry { root:"r".into(), path:"file.rs".into(), file_name:"file.rs".into(), match_type:"file".into() }]
        })
    }));
    service.start_session("s".into(), vec!["r".into()]).unwrap();
    service.update_session("s".into(), "old".into()).unwrap();
    service.update_session("s".into(), "file".into()).unwrap();
    ready.send(()).unwrap();
    let updated = tokio::time::timeout(Duration::from_secs(5), receive.recv()).await.unwrap().unwrap();
    assert_eq!(updated["method"], "fuzzyFileSearch/sessionUpdated");
    assert_eq!(updated["params"]["query"], "file");
    assert_eq!(updated["params"]["files"][0]["path"], "file.rs");
    let completed = tokio::time::timeout(Duration::from_secs(5), receive.recv()).await.unwrap().unwrap();
    assert_eq!(completed["method"], "fuzzyFileSearch/sessionCompleted");
    service.stop_session("s");
    assert_eq!(service.update_session("s".into(), "x".into()).unwrap_err().code, -32600);
    service.dispose();
}

#[tokio::test]
async fn replacement_token_cancels_previous_search_without_removing_new_search() {
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let service = FuzzyFileSearchService::with_collector(Arc::new(|_| {}), Arc::new(move |_, cancelled| {
        let (send, receive) = tokio::sync::oneshot::channel();
        events.send((cancelled, send)).unwrap();
        Box::pin(async move { receive.await.unwrap(); Vec::new() })
    }));
    let first_service = service.clone();
    let first = tokio::spawn(async move { first_service.search("a", vec![], Some("same".into())).await });
    let (first_cancelled, first_done) = tokio::time::timeout(Duration::from_secs(5), received.recv()).await.unwrap().unwrap();
    let second_service = service.clone();
    let second = tokio::spawn(async move { second_service.search("b", vec![], Some("same".into())).await });
    let (second_cancelled, second_done) = tokio::time::timeout(Duration::from_secs(5), received.recv()).await.unwrap().unwrap();
    assert!(first_cancelled.load(Ordering::Acquire));
    assert!(!second_cancelled.load(Ordering::Acquire));
    first_done.send(()).unwrap();
    assert!(first.await.unwrap().is_empty());
    service.dispose();
    assert!(second_cancelled.load(Ordering::Acquire));
    second_done.send(()).unwrap();
    assert!(second.await.unwrap().is_empty());
}

#[tokio::test]
async fn registry_preserves_experimental_gate_and_parameter_validation() {
    let service = FuzzyFileSearchService::new(Arc::new(|_| {}));
    let mut registry = MethodRegistry::default();
    register_fuzzy_file_search_methods(&mut registry, service.clone());
    let connection = RegistryConnection { initialized:true, ..Default::default() };
    let response = registry.dispatch(connection.clone(), json!({"id":1,"method":"fuzzyFileSearch","params":{"query":"","roots":[]}})).await;
    assert_eq!(response["result"], json!({"files":[]}));
    let response = registry.dispatch(connection.clone(), json!({"id":2,"method":"fuzzyFileSearch","params":{"query":"x","roots":[1]}})).await;
    assert_eq!(response["error"]["code"], -32600);
    let response = registry.dispatch(connection, json!({"id":3,"method":"fuzzyFileSearch/sessionStart","params":{"sessionId":"s","roots":[]}})).await;
    assert_eq!(response["error"]["code"], -32600);
    service.dispose();
}
