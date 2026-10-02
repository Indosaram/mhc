use maho_server::app_server::{envelope::classify_incoming, server_core::ServerCore};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn initialize_dispatch_correlates_errors_and_gates_notifications() {
    let mut core = ServerCore::new("/tmp/home".into(), "1".into(), "Linux".into(), "test".into(), "x64".into(), "linux".into());
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    core.add_connection("client".into(), Arc::new(move |message| { send.send(message).unwrap(); Box::pin(async { Ok(()) }) }));
    core.receive("client", classify_incoming(json!({"id":1,"method":"unknown"}))).await.unwrap();
    let response = receive.try_recv().unwrap();
    assert_eq!(response["id"], 1);
    assert_eq!(response["error"]["message"], "Not initialized");
    core.receive("client", classify_incoming(json!({"id":2,"method":"initialize","params":{"clientInfo":{"name":"c","version":"1"}}}))).await.unwrap();
    assert_eq!(receive.try_recv().unwrap()["result"]["codexHome"], "/tmp/home");
    core.receive("client", classify_incoming(json!({"id":3,"method":"initialize","params":{}}))).await.unwrap();
    assert_eq!(receive.try_recv().unwrap()["error"]["message"], "Already initialized");
    assert_eq!(core.broadcast_notification(json!({"method":"turn/started"}), 123).await.unwrap(), 1);
    assert_eq!(receive.try_recv().unwrap()["emittedAtMs"], 123);
    core.remove_connection("client");
    assert_eq!(core.broadcast_notification(json!({"method":"turn/started"}), 124).await.unwrap(), 0);
}

#[tokio::test]
async fn history_and_search_methods_require_experimental_capability() {
    use maho_server::app_server::{runtime::AppServerRuntime,registry::RegistryConnection};
    let directory = tempfile::tempdir().unwrap();
    let runtime = AppServerRuntime::new(directory.path().display().to_string(),directory.path().display().to_string(),"1".into(),Some(directory.path().display().to_string()),None).await;
    for method in ["thread/search","thread/searchOccurrences","thread/turns/list","thread/items/list"] {
        let response = runtime.core.read().await.registry.dispatch(RegistryConnection {initialized:true,..Default::default()},json!({"id":1,"method":method,"params":{}})).await;
        assert_eq!(response["error"]["code"],-32600);
    }
    runtime.dispose().await;
}
