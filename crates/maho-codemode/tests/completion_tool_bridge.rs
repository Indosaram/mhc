use maho_codemode::completion::{tool_bridge::*,handler::CompletionError};
use serde_json::json;
use std::sync::Mutex;

#[tokio::test] async fn structured_value_including_null_wins_over_text() {
    let replies=Mutex::new(vec![]);
    let summary=handle_completion_tool_call("call",&json!({"prompt":"x"}), |_| async { Ok(json!({"value":null,"text":"unused"})) },||true,|reply| replies.lock().unwrap().push(reply)).await;
    assert!(summary.ok);
    assert_eq!(replies.lock().unwrap()[0],json!({"type":"tool-reply","callId":"call","ok":true,"value":null}));
}
#[tokio::test] async fn finalized_cell_receives_no_reply() {
    let replies=Mutex::new(vec![]);
    let summary=handle_completion_tool_call("call",&json!({"prompt":"x"}), |_| async { Ok(json!({"text":"result"})) },||false,|reply| replies.lock().unwrap().push(reply)).await;
    assert!(!summary.ok);assert!(replies.lock().unwrap().is_empty());
}
#[tokio::test] async fn failure_is_replied_only_to_active_cell() {
    let replies=Mutex::new(vec![]);
    let summary=handle_completion_tool_call("call",&json!({"prompt":"x"}), |_| async { Err(CompletionError("failure".into())) },||true,|reply| replies.lock().unwrap().push(reply)).await;
    assert_eq!(summary.error.as_deref(),Some("failure"));
    assert_eq!(replies.lock().unwrap()[0]["error"]["message"],"failure");
}
#[tokio::test] async fn invalid_args_do_not_invoke_completion() {
    let summary=handle_completion_tool_call("call",&json!({}), |_| async { panic!("must not execute") },||true,|_|{}).await;
    assert!(!summary.ok);
    assert!(to_completion_request(&json!({"prompt":"","model":false,"schema":null})).unwrap().schema.is_some());
}
