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

#[test]
fn projected_items_carry_started_and_completed_timestamps() {
    let mut projector = EventProjector::with_clock("thread".into(), "turn".into(), "/tmp".into(), std::sync::Arc::new(|| 42_i64));
    let message = json!({"role":"assistant","responseId":"response"});
    let started = projector.project(&json!({"type":"message_update","message":message,"assistantMessageEvent":{"type":"text_start","contentIndex":0}}));
    assert_eq!(started.notifications[0]["params"]["startedAtMs"], 42);
    let completed = projector.project(&json!({"type":"message_update","message":message,"assistantMessageEvent":{"type":"text_end","contentIndex":0,"content":"hi"}}));
    assert_eq!(completed.notifications[0]["params"]["completedAtMs"], 42);
    let tool = projector.project(&json!({"type":"message_update","message":message,"assistantMessageEvent":{"type":"toolcall_end","toolCall":{"id":"tool","name":"bash","arguments":{"command":"ls"}}}}));
    assert_eq!(tool.notifications[0]["params"]["startedAtMs"], 42);
    let compaction = projector.project(&json!({"type":"compaction_start"}));
    assert_eq!(compaction.notifications[0]["params"]["startedAtMs"], 42);
}

#[test]
fn finalize_completes_dangling_tools_and_compaction_uses_upstream_id() {
    let mut projector = EventProjector::new("thread".into(), "turn".into(), "/tmp".into());
    let message = json!({"role":"assistant","responseId":"response"});
    projector.project(&json!({"type":"message_update","message":message,"assistantMessageEvent":{"type":"toolcall_end","toolCall":{"id":"tool","name":"custom","arguments":{}}}}));
    let compaction = projector.project(&json!({"type":"compaction_start"}));
    assert_eq!(compaction.notifications[0]["params"]["item"]["id"], "context-compaction:1");
    let finalized = projector.finalize();
    let tool = finalized.iter().find(|notification| notification["params"]["item"]["id"] == "tool").unwrap();
    assert_eq!(tool["params"]["item"]["status"], "completed");
}
