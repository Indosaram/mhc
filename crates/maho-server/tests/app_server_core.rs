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
