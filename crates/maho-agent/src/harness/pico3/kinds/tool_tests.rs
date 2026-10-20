use super::*;

fn call(value: serde_json::Value) -> StoredToolCall { Stored::from_json(json(&value).expect("call JSON")) }
fn text(result: &ToolResult) -> String {
    result.content.iter().flatten().filter_map(|b| match b { ToolContent::Text { text } => Some(text.as_str()), ToolContent::Image { .. } => None }).collect()
}

#[test]
fn replay_requires_both_declarations_to_be_safe() {
    let cases = [(Replay::Safe, Replay::Safe, true), (Replay::Safe, Replay::Unsafe, false), (Replay::Unsafe, Replay::Safe, false), (Replay::Unsafe, Replay::Unsafe, false)];
    for (old, new, expected) in cases { assert_eq!(replay_allowed(old, new), expected); }
}

#[test]
fn rewritten_arguments_preserve_identity_but_namespace_changes_do_not() {
    let original = call(serde_json::json!({"id":"a","name":"x","namespace":"n","arguments":{"v":"old"}}));
    let rewritten = call(serde_json::json!({"id":"a","name":"x","namespace":"n","arguments":[1,2]}));
    let renamed = call(serde_json::json!({"id":"a","name":"x","namespace":"other","arguments":{}}));
    assert!(same_identity(&original, &rewritten).expect("identity"));
    assert!(!same_identity(&original, &renamed).expect("identity"));
    assert_eq!(arguments(&rewritten).expect("arguments"), json(&serde_json::json!([1,2])).expect("JSON"));
}

#[test]
fn returned_blocks_share_aggregate_budget_and_keep_images() {
    let result = ToolResult { content: Some(vec![ToolContent::Text { text: "abcdefgh".into() }, ToolContent::Image { data: "image".into(), mime_type: "image/png".into() }, ToolContent::Text { text: "ijklmnop".into() }, ToolContent::Text { text: "qrstuvwx".into() }]), ..ToolResult::default() };
    let (bounded, truncated) = bound(result, &OutputLimits { max_bytes: Some(10), max_lines: Some(100), retain: Some(Retain::Tail) });
    assert_eq!(text(&bounded), "opqrstuvwx");
    assert_eq!(truncated.bytes, 14);
    assert_eq!(bounded.content.as_ref().expect("content").len(), 2);
    assert!(matches!(bounded.content.as_ref().expect("content").first(), Some(ToolContent::Image { .. })));
}

#[test]
fn line_budget_precedes_byte_budget_for_both_retention_modes() {
    for (retain, expected) in [(Retain::Head, "a\nb"), (Retain::Tail, "c\nd")] {
        let result = ToolResult { content: Some(vec![ToolContent::Text { text: "a\nb\nc\nd".into() }]), ..ToolResult::default() };
        let (bounded, truncated) = bound(result, &OutputLimits { max_bytes: Some(3), max_lines: Some(2), retain: Some(retain) });
        assert_eq!(text(&bounded), expected);
        assert_eq!((truncated.bytes, truncated.lines), (0, 2));
    }
}

#[test]
fn zero_output_budgets_retain_no_returned_text() {
    for retain in [Retain::Head, Retain::Tail] {
        let result = ToolResult { content: Some(vec![ToolContent::Text { text: "abcdef".into() }]), ..ToolResult::default() };
        let (bounded, _) = bound(result, &OutputLimits { max_bytes: Some(0), max_lines: Some(0), retain: Some(retain) });
        assert_eq!(text(&bounded), "");
    }
}

#[test]
fn result_message_uses_stored_call_identity_and_preserves_controls_in_result() {
    let call = call(serde_json::json!({"id":"final-id","name":"final-name","arguments":false}));
    let result = ToolResult { is_error: Some(true), control: Some(ToolControl { terminate: Some(True), handoff: Some("handoff".into()), add_tools: Some(vec!["next".into()]) }), ..ToolResult::default() };
    let entry = result_entry(&call, &result, 42).expect("entry");
    let model: JsonObject = decode(entry.model.as_ref().expect("model")[0].as_json()).expect("model JSON");
    assert_eq!(model.get("toolCallId"), Some(&json(&"final-id").expect("id")));
    assert_eq!(model.get("toolName"), Some(&json(&"final-name").expect("name")));
    assert_eq!(model.get("timestamp"), Some(&json(&42).expect("timestamp")));
    assert_eq!(model.get("isError"), Some(&json(&true).expect("error")));
}

struct StrictDeclaration { parameters: JsonObject }
impl ToolDeclaration for StrictDeclaration {
    type Args = JsonValue;
    fn name(&self) -> &str { "strict" }
    fn description(&self) -> &str { "" }
    fn parameters(&self) -> &JsonObject { &self.parameters }
    fn replay(&self) -> Replay { Replay::Safe }
    fn output(&self) -> OutputLimits { OutputLimits::default() }
    fn execute<'a>(&'a self, _: JsonValue, _: &'a dyn ToolApi, _: &'a Context) -> LocalBoxFuture<'a, Result<ToolResult>> { Box::pin(async { panic!("validation must not execute") }) }
}

#[test]
fn schema_validation_rejects_null_and_wrong_type_without_coercion() {
    let declaration = StrictDeclaration { parameters: decode(&json(&serde_json::json!({"type":"object","properties":{"v":{"type":"string"}},"required":["v"]})).expect("schema")).expect("object") };
    for args in [serde_json::json!({"v":null}), serde_json::json!({"v":42}), serde_json::json!({})] {
        let call = call(serde_json::json!({"id":"a","name":"strict","arguments":args}));
        assert!(invalid(&declaration, &call).expect("validation").is_some());
    }
    let call = call(serde_json::json!({"id":"a","name":"strict","arguments":{"v":"ok"}}));
    assert!(invalid(&declaration, &call).expect("validation").is_none());
}
