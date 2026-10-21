use super::*;

fn output() -> JsonObject {
    serde_json::from_str(r#"{"message":{"content":[{"type":"toolCall","id":"old","name":"run","arguments":{"keep":1}}]}}"#).expect("output")
}
fn arguments(output: &JsonObject) -> JsonValue {
    let Some(JsonValue::Object(message)) = output.get("message") else { panic!("message"); };
    let Some((_, JsonValue::Array(blocks))) = message.iter().find(|(key, _)| key == "content") else { panic!("content"); };
    let Some(JsonValue::Object(block)) = blocks.first() else { panic!("block"); };
    block.iter().find(|(key, _)| key == "arguments").expect("arguments").1.clone()
}

#[test]
fn checkpoint_clears_arguments_when_json_is_incomplete() {
    let mut tracked = output();
    apply_frame(&mut tracked, &AssistantMessageFrame::ToolcallCheckpoint { content_index: 0, json: r#"{"unfinished":"#.into() }).expect("frame");
    assert_eq!(arguments(&tracked), JsonValue::object(Vec::new()));
}

#[test]
fn checkpoint_preserves_valid_nonobject_json_when_null() {
    let mut tracked = output();
    apply_frame(&mut tracked, &AssistantMessageFrame::ToolcallCheckpoint { content_index: 0, json: "null".into() }).expect("frame");
    assert_eq!(arguments(&tracked), JsonValue::Null);
}

#[test]
fn delta_does_not_change_arguments_when_toolcall_streams() {
    let mut tracked = output();
    let before = tracked.clone();
    apply_frame(&mut tracked, &AssistantMessageFrame::ToolcallDelta { content_index: 0, delta: "{invalid".into() }).expect("frame");
    assert_eq!(tracked, before);
}

#[test]
fn rejects_delta_when_no_start_message_exists() {
    let mut tracked = JsonObject::new();
    let result = apply_frame(&mut tracked, &AssistantMessageFrame::ToolcallDelta { content_index: 0, delta: "x".into() });
    assert!(matches!(result, Err(Error::InvalidShape(_))));
}

#[test]
fn text_end_removes_stale_signature_when_omitted() {
    let mut tracked: JsonObject = serde_json::from_str(r#"{"message":{"content":[{"type":"text","text":"old","textSignature":"stale"}]}}"#).expect("output");
    apply_frame(&mut tracked, &AssistantMessageFrame::TextEnd { content_index: 0, content: "final".into(), text_signature: None }).expect("frame");
    let expected: JsonObject = serde_json::from_str(r#"{"message":{"content":[{"type":"text","text":"final"}]}}"#).expect("expected");
    assert_eq!(tracked, expected);
}

#[test]
fn toolcall_start_retains_discriminator_when_block_is_inserted() {
    let mut tracked: JsonObject = serde_json::from_str(r#"{"message":{"content":[]}}"#).expect("output");
    let frame: AssistantMessageFrame = serde_json::from_str(r#"{"type":"toolcall_start","contentIndex":0,"toolCall":{"id":"a","name":"run","arguments":{}}}"#).expect("frame");
    apply_frame(&mut tracked, &frame).expect("apply");
    let expected: JsonObject = serde_json::from_str(r#"{"message":{"content":[{"type":"toolCall","id":"a","name":"run","arguments":{}}]}}"#).expect("expected");
    assert_eq!(tracked, expected);
}

#[test]
fn toolcall_end_retains_signature_when_end_omits_it() {
    let mut tracked: JsonObject = serde_json::from_str(r#"{"message":{"content":[{"type":"toolCall","id":"a","name":"run","arguments":{},"thoughtSignature":"keep","namespace":"keep.ns"}]}}"#).expect("output");
    let frame: AssistantMessageFrame = serde_json::from_str(r#"{"type":"toolcall_end","contentIndex":0,"id":"b","name":"done","arguments":{"x":1}}"#).expect("frame");
    apply_frame(&mut tracked, &frame).expect("apply");
    let expected: JsonObject = serde_json::from_str(r#"{"message":{"content":[{"type":"toolCall","id":"b","name":"done","arguments":{"x":1},"thoughtSignature":"keep","namespace":"keep.ns"}]}}"#).expect("expected");
    assert_eq!(tracked, expected);
}

#[test]
fn thinking_end_clears_redaction_and_signature_when_omitted() {
    let mut tracked: JsonObject = serde_json::from_str(r#"{"message":{"content":[{"type":"thinking","thinking":"old","thinkingSignature":"stale","redacted":true}]}}"#).expect("output");
    apply_frame(&mut tracked, &AssistantMessageFrame::ThinkingEnd { content_index: 0, content: "final".into(), thinking_signature: None, redacted: None }).expect("apply");
    let expected: JsonObject = serde_json::from_str(r#"{"message":{"content":[{"type":"thinking","thinking":"final"}]}}"#).expect("expected");
    assert_eq!(tracked, expected);
}
