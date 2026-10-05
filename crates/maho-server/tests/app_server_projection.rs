use maho_server::app_server::projection::EventProjector;
use serde_json::json;
#[test]
fn assistant_done_does_not_finish_pending_tool_and_turn_finalize_is_idempotent() {
    let mut projector = EventProjector::new("thread".into(), "turn".into(), "/tmp".into());
    let message = json!({"role":"assistant","responseId":"response"});
    projector.project(&json!({"type":"message_start","message":message}));
    let start = projector.project(&json!({"type":"message_update","message":message,"assistantMessageEvent":{"type":"text_start","contentIndex":0}}));
    assert_eq!(start.notifications[0]["params"]["item"]["id"], "response:0");
    projector.project(&json!({"type":"message_update","message":message,"assistantMessageEvent":{"type":"toolcall_end","toolCall":{"id":"tool","name":"bash","arguments":{"command":"ls"}}}}));
    let done = projector.project(&json!({"type":"message_update","message":message,"assistantMessageEvent":{"type":"done"}}));
    assert_eq!(done.turn_completion.unwrap().status, "completed");
    assert!(done.notifications.iter().all(|message| message["params"]["item"]["id"] != "tool"));
    let end = projector.project(&json!({"type":"tool_execution_end","toolCallId":"tool","isError":false,"result":{"content":[{"type":"text","text":"output"}]}}));
    assert_eq!(end.notifications[0]["params"]["item"]["aggregatedOutput"], "output");
    assert!(projector.finalize().is_empty());
    assert!(projector.finalize().is_empty());
    assert!(projector.project(&json!({"type":"compaction_start"})).notifications.is_empty());
}
