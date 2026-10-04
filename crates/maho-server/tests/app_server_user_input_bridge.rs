use maho_server::app_server::user_input_bridge::UserInputBridge;
use maho_ext_api::{Question,QuestionOption,QuestionRequest,QuestionOptions,QuestionStatus};
use serde_json::json;
use std::sync::{Arc,Mutex};

fn request() -> QuestionRequest {QuestionRequest {request_id:"question".into(),questions:vec![Question {id:"choice".into(),header:"Choice".into(),question:"Pick".into(),options:vec![QuestionOption {label:"One".into(),description:None}],multi_select:false}],wait_for_answer:true,timeout_ms:10000}}
#[tokio::test]
async fn user_input_preserves_drafts_replays_and_resolves_only_once() {
    let (send,mut receive) = tokio::sync::mpsc::unbounded_channel();
    let bridge = Arc::new(Mutex::new(UserInputBridge::new(Arc::new(move |_,message| {send.send(message).unwrap();1}))));
    let result = UserInputBridge::request_user_input(&bridge,"thread","turn","item",request(),QuestionOptions::default());
    let outbound = receive.try_recv().unwrap();assert_eq!(outbound["id"],"user-input-0");
    assert_eq!(outbound["params"]["questions"][0]["isSecret"],false);
    assert_eq!(bridge.lock().unwrap().replay_pending_for_thread("thread"),1);assert_eq!(receive.try_recv().unwrap(),outbound);
    assert!(bridge.lock().unwrap().progress(&json!({"requestId":"user-input-0","answers":{"choice":{"answers":["One","Other"]}},"comment":"draft"})).unwrap());
    assert!(bridge.lock().unwrap().resolve_response(&json!({"id":"user-input-0","result":{"cancelled":true,"answers":{"choice":{"answers":["One"]}}}})).unwrap());
    let answer = result.await.unwrap();assert_eq!(answer.status,QuestionStatus::Cancelled);assert_eq!(answer.answers["choice"].selected,["One"]);
    assert_eq!(receive.try_recv().unwrap()["method"],"serverRequest/resolved");
    assert!(!bridge.lock().unwrap().resolve_response(&json!({"id":"user-input-0","result":{}})).unwrap());
    assert_eq!(bridge.lock().unwrap().pending_count(),0);
}
#[tokio::test]
async fn absent_subscribers_abort_and_zero_timeout_settle_without_orphans() {
    let bridge = Arc::new(Mutex::new(UserInputBridge::new(Arc::new(|_,_|0))));
    let unavailable = UserInputBridge::request_user_input(&bridge,"thread","turn","item",request(),QuestionOptions::default()).await.unwrap();
    assert_eq!(unavailable.status,QuestionStatus::Unavailable);
    let signal = maho_ext_api::AbortSignal::default();signal.abort();
    let cancelled = UserInputBridge::request_user_input(&bridge,"thread","turn","item",request(),QuestionOptions {dialog:maho_ext_api::ExtensionUiDialogOptions {signal:Some(signal),timeout_ms:None},..Default::default()}).await.unwrap();
    assert_eq!(cancelled.status,QuestionStatus::Cancelled);
    let timed = UserInputBridge::request_user_input(&bridge,"thread","turn","item",request(),QuestionOptions {dialog:maho_ext_api::ExtensionUiDialogOptions {signal:None,timeout_ms:Some(0)},..Default::default()}).await.unwrap();
    assert_eq!(timed.status,QuestionStatus::TimedOut);assert_eq!(bridge.lock().unwrap().pending_count(),0);
}
#[tokio::test]
async fn user_input_malformed_result_keeps_pending_and_abort_subscription_settles_it() {
    let bridge = Arc::new(Mutex::new(UserInputBridge::new(Arc::new(|_,_|1))));
    let signal = maho_ext_api::AbortSignal::default();
    let result = UserInputBridge::request_user_input(&bridge,"thread","turn","item",request(),QuestionOptions {dialog:maho_ext_api::ExtensionUiDialogOptions {signal:Some(signal.clone()),timeout_ms:None},..Default::default()});
    assert!(bridge.lock().unwrap().resolve_response(&json!({"id":"user-input-0","result":{"answers":false}})).is_err());
    assert_eq!(bridge.lock().unwrap().pending_count(),1);
    signal.abort();
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(3),result).await.unwrap().unwrap().status,QuestionStatus::Cancelled);
    assert_eq!(bridge.lock().unwrap().pending_count(),0);
}
