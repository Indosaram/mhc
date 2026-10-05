use maho_server::app_server::{connection::TransportKind,envelope::classify_incoming,server_core::{ConnectionInput,ServerCore}};
use serde_json::json;
use std::sync::{Arc,atomic::{AtomicBool,Ordering}};

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
async fn connection_input_records_transport_kind_and_invokes_close_callback() {
    let mut core = ServerCore::new("/tmp/home".into(), "1".into(), "Linux".into(), "test".into(), "x64".into(), "linux".into());
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let closed = Arc::new(AtomicBool::new(false));
    let close_flag = closed.clone();
    let connection = core.add_connection_input(ConnectionInput { id:"ws".into(), transport_kind:TransportKind::WebSocket,
        send:Arc::new(move |message| { send.send(message).unwrap(); Box::pin(async { Ok(()) }) }),
        close:Some(Arc::new(move |reason| { assert_eq!(reason, "slow-client"); close_flag.store(true, Ordering::SeqCst); })) });
    assert_eq!(connection.transport_kind, TransportKind::WebSocket);
    core.receive("ws", classify_incoming(json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"qa","version":"1"}}}))).await.unwrap();
    assert_eq!(receive.try_recv().unwrap()["id"], 1);
    assert!(core.close_connection("ws", "slow-client"));
    assert!(closed.load(Ordering::SeqCst));
    assert!(!core.close_connection("missing", "slow-client"));
    core.remove_connection("ws");
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

#[tokio::test]
async fn runtime_subscribes_to_provider_account_events_and_unsubscribes_on_dispose() {
    use maho_server::app_server::runtime::AppServerRuntime;
    let directory = tempfile::tempdir().unwrap();
    let runtime = AppServerRuntime::new(directory.path().display().to_string(),directory.path().display().to_string(),"1".into(),Some(directory.path().display().to_string()),None).await;
    let (send,mut receive) = tokio::sync::mpsc::unbounded_channel();
    runtime.core.write().await.add_connection("qa".into(),Arc::new(move |message| {send.send(message).unwrap();Box::pin(async {Ok(())})}));
    runtime.core.read().await.receive("qa",classify_incoming(json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"qa","version":"1"}}}))).await.unwrap();
    receive.try_recv().unwrap();
    maho_core::emit_provider_accounts_changed("app-server-fixture-provider");
    let notification = tokio::time::timeout(std::time::Duration::from_secs(5),async {
        loop {
            let message = receive.recv().await.expect("connection alive");
            if message["method"] == "account/providerAccounts/updated" { break message; }
        }
    }).await.expect("provider event within deadline");
    assert_eq!(notification["params"]["provider"],"app-server-fixture-provider");
    runtime.dispose().await;
    maho_core::emit_provider_accounts_changed("app-server-fixture-provider");
    assert!(tokio::time::timeout(std::time::Duration::from_millis(250),receive.recv()).await.is_err(),"dispose must unsubscribe the provider account listener");
}

#[tokio::test]
async fn initialized_core_routes_user_input_responses_and_reports_unknown_ids() {
    use maho_server::app_server::user_input_bridge::UserInputBridge;
    use maho_ext_api::{QuestionRequest,QuestionOptions};
    let mut core = ServerCore::new("/tmp/home".into(),"1".into(),"Linux".into(),"test".into(),"x64".into(),"linux".into());
    let bridge = Arc::new(std::sync::Mutex::new(UserInputBridge::new(Arc::new(|_,_|1))));
    core.user_input = Some(bridge.clone());
    let (send,mut receive) = tokio::sync::mpsc::unbounded_channel();
    core.add_connection("qa".into(),Arc::new(move |message| {send.send(message).unwrap();Box::pin(async {Ok(())})}));
    core.receive("qa",classify_incoming(json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"qa","version":"1"}}}))).await.unwrap();
    receive.try_recv().unwrap();
    let answered = UserInputBridge::request_user_input(&bridge,"thread","turn","item",QuestionRequest {request_id:"q".into(),questions:Vec::new(),wait_for_answer:true,timeout_ms:10000},QuestionOptions::default());
    core.receive("qa",classify_incoming(json!({"id":"user-input-0","result":{"answers":false}}))).await.unwrap();
    assert_eq!(receive.try_recv().unwrap()["error"]["code"],-32602);
    core.receive("qa",classify_incoming(json!({"id":"user-input-0","result":{"answers":{}}}))).await.unwrap();
    assert_eq!(answered.await.unwrap().status,maho_ext_api::QuestionStatus::Answered);
    core.receive("qa",classify_incoming(json!({"id":"user-input-0","result":{}}))).await.unwrap();
    assert_eq!(receive.try_recv().unwrap()["error"]["code"],-32600);
    assert_eq!(bridge.lock().unwrap().pending_count(),0);
    core.remove_connection("qa");
}
