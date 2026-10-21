use super::*;
use super::super::memory::MemoryStorage;
use crate::harness::context::BACKGROUND_CONTEXT;

fn writes(source: &str) -> Vec<Write> { serde_json::from_str(source).expect("fixture") }
fn messages(source: &str) -> Vec<StoredMessage> { serde_json::from_str(source).expect("messages") }

#[tokio::test]
async fn inherited_heads_inner_heads_display_only_and_newest_edits_are_preserved() {
    let storage = MemoryStorage::new();
    let ctx = &BACKGROUND_CONTEXT;
    storage.commit(&writes(r#"[
        {"type":"conversation","conversation":{"id":1}},
        {"type":"entry","entry":{"id":2,"conversationId":1,"kind":"user","model":[{"role":"user","content":"original","timestamp":1}]}},
        {"type":"entry","entry":{"id":3,"conversationId":1,"kind":"summary","head":2}},
        {"type":"entry","entry":{"id":4,"conversationId":1,"kind":"display","data":{"reason":"aborted"}}},
        {"type":"entry","entry":{"id":5,"conversationId":1,"kind":"edit","edits":[{"target":2,"action":"omit"}]}},
        {"type":"entry","entry":{"id":6,"conversationId":1,"kind":"edit","edits":[{"target":2,"action":"replace","messages":[{"role":"user","content":"newest","timestamp":2}]}]}},
        {"type":"entry","entry":{"id":7,"conversationId":1,"kind":"invisible","model":[{"role":"user","content":"past fork","timestamp":3}]}},
        {"type":"conversation","conversation":{"id":10,"parent":{"conversationId":1,"at":6}}},
        {"type":"entry","entry":{"id":11,"conversationId":10,"kind":"handoff","head":2}}
    ]"#), ctx).await.expect("commit");
    let context = derive_context(&storage, 10, None, ctx).await.expect("context");
    assert_eq!(context.head.expect("head").id, 11);
    assert_eq!(context.entries.iter().map(|entry| entry.id).collect::<Vec<_>>(), vec![11, 2, 4, 5, 6]);
    assert_eq!(context.messages.len(), 1);
    assert_eq!(context.messages[0].as_json().get("content").and_then(JsonValue::as_str), Some("newest"));
    assert_eq!(storage.entries(&[2], ctx).await.expect("read")[&2].model.as_ref().expect("model")[0].as_json().get("content").and_then(JsonValue::as_str), Some("original"));
    let earlier = derive_context(&storage, 10, Some(5), ctx).await.expect("earlier");
    assert!(earlier.messages.is_empty());
}

#[tokio::test]
async fn pagination_has_no_fixed_page_cap_and_stops_at_head_target() {
    let storage = MemoryStorage::new();
    let ctx = &BACKGROUND_CONTEXT;
    let mut batch = writes(r#"[{"type":"conversation","conversation":{"id":1}}]"#);
    for id in 2..=602 {
        batch.push(Write::Entry { entry: Entry { id, conversation_id: 1, kind: "history".into(), model: None, data: None, head: None, edits: None, by_task_id: None } });
    }
    batch.extend(writes(r#"[{"type":"entry","entry":{"id":603,"conversationId":1,"kind":"summary","head":100}}]"#));
    storage.commit(&batch, ctx).await.expect("commit");
    let context = derive_context(&storage, 1, None, ctx).await.expect("context");
    assert_eq!(context.entries.len(), 504);
    assert_eq!(context.entries[0].id, 603);
    assert_eq!(context.entries[1].id, 100);
    assert_eq!(context.entries.last().expect("last").id, 602);
}

#[test]
fn tool_results_follow_call_order_and_missing_results_preserve_lossless_ids() {
    let input = messages(r#"[
        {"role":"assistant","content":[{"type":"toolCall","id":"\ud800","name":"first"},{"type":"toolCall","id":"b","name":"second"},{"type":"toolCall","id":"c","name":"third"}],"timestamp":12},
        {"role":"toolResult","toolCallId":"c","toolName":"third","content":[],"timestamp":13},
        {"role":"toolResult","toolCallId":"\ud800","toolName":"first","content":[],"timestamp":14},
        {"role":"user","content":"next","timestamp":15}
    ]"#);
    let ordered = reorder_tool_results(input.clone());
    assert_eq!(ordered.len(), 5);
    assert_eq!(ordered[1].as_json(), input[2].as_json());
    assert_eq!(ordered[3].as_json(), input[1].as_json());
    assert_eq!(ordered[2].as_json().get("toolCallId").and_then(JsonValue::as_str), Some("b"));
    assert_eq!(ordered[2].as_json().get("details").and_then(|details| details.get("reason")).and_then(JsonValue::as_str), Some("missing_after_fork"));
    assert_eq!(ordered[2].as_json().get("timestamp"), input[0].as_json().get("timestamp"));
    assert_eq!(ordered[4].as_json(), input[3].as_json());
}

#[test]
fn no_call_assistants_leave_following_messages_untouched() {
    let input = messages(r#"[{"role":"assistant","content":[{"type":"text","text":"done"}],"timestamp":1},{"role":"toolResult","toolCallId":"standalone","content":[],"timestamp":2}]"#);
    let output = reorder_tool_results(input.clone());
    assert_eq!(output.iter().map(StoredMessage::as_json).collect::<Vec<_>>(), input.iter().map(StoredMessage::as_json).collect::<Vec<_>>());
}
