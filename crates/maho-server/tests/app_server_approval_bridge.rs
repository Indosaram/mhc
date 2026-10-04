use maho_server::app_server::{approval_bridge::ApprovalBridge, approval_types::*};
use serde_json::json;
use std::sync::Arc;
#[tokio::test]
async fn approval_first_response_wins_and_session_allow_is_scoped_by_thread_and_command() {
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let mut bridge = ApprovalBridge::new(Arc::new(move |_, request| { send.send(request).unwrap(); 1 }));
    let payload = json!({"toolName":"bash","command":"ls"});
    let outcome = bridge.request_approval("thread", ApprovalKind::CommandExecution, &payload, 42);
    assert_eq!(receive.try_recv().unwrap()["params"]["startedAtMs"], 42);
    assert_eq!(bridge.replay_pending_for_thread("thread"), 1);
    receive.try_recv().unwrap();
    assert!(bridge.resolve_response(&json!({"id":0,"result":{"decision":"acceptForSession"}})));
    assert!(!bridge.resolve_response(&json!({"id":0,"result":{"decision":"decline"}})));
    assert!(outcome.await.unwrap().allow);
    assert_eq!(receive.try_recv().unwrap()["method"], "serverRequest/resolved");
    assert!(bridge.request_approval("thread", ApprovalKind::CommandExecution, &payload, 43).await.unwrap().allow);
    assert!(receive.try_recv().is_err());
    let other = bridge.request_approval("other", ApprovalKind::CommandExecution, &payload, 44);
    assert_eq!(bridge.cancel_pending_for_thread("other"), 1);
    assert_eq!(other.await.unwrap().decision, ApprovalDecision::Cancel);
    assert_eq!(bridge.pending_count(), 0);
}
#[tokio::test]
async fn absent_subscriber_declines_without_retaining_pending_request() {
    let mut bridge = ApprovalBridge::new(Arc::new(|_, _| 0));
    let outcome = bridge.request_approval("thread", ApprovalKind::FileChange, &json!({}), 0).await.unwrap();
    assert!(!outcome.allow);
    assert_eq!(outcome.decision, ApprovalDecision::Decline);
    assert_eq!(bridge.pending_count(), 0);
}
