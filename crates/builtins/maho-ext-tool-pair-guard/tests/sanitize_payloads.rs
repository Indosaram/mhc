use maho_ext_tool_pair_guard::sanitize_openai_chat_completions_payload::sanitize_openai_chat_completions_payload as chat;
use maho_ext_tool_pair_guard::sanitize_openai_responses_payload::sanitize_openai_responses_payload as responses;
use serde_json::{Value, json};

fn assistant(ids: &[&str]) -> Value {
    json!({"role":"assistant", "content":null, "tool_calls":ids.iter().map(|id| json!({"id":id,"type":"function","function":{"name":"bash","arguments":"{}"}})).collect::<Vec<_>>()})
}
fn output(id: &str) -> Value { json!({"role":"tool","tool_call_id":id,"content":"ok"}) }

#[test]
fn chat_without_messages_when_unrelated_payload() {
    let payload = json!({});
    assert_eq!(chat(&payload), payload);
}
#[test]
fn chat_nonobjects_when_no_message_array() {
    for payload in [Value::Null, json!("text"), json!(42)] { assert_eq!(chat(&payload), payload); }
}
#[test]
fn chat_valid_pairs_when_complete() {
    let payload = json!({"messages":[assistant(&["a"]),output("a")]});
    assert_eq!(chat(&payload), payload);
}
#[test]
fn chat_orphan_when_no_call() {
    let payload = json!({"messages":[output("missing")]});
    assert_eq!(chat(&payload)["messages"], json!([]));
}
#[test]
fn chat_missing_result_when_orphan_follows() {
    let payload = json!({"messages":[assistant(&["a"]),output("orphan")]});
    let result = chat(&payload);
    assert_eq!(result["messages"][1]["tool_call_id"], "a");
    assert_eq!(result["messages"].as_array().unwrap().len(), 2);
}
#[test]
fn chat_keeps_paired_when_orphan_present() {
    let payload = json!({"messages":[assistant(&["a"]),output("a"),output("b")]});
    assert_eq!(chat(&payload)["messages"], json!([assistant(&["a"]),output("a")]));
}
#[test]
fn chat_synthesizes_remaining_when_partial_results() {
    let payload = json!({"messages":[assistant(&["a","b"]),output("a")]});
    let result = chat(&payload);
    assert_eq!(result["messages"][2]["tool_call_id"], "b");
    assert_eq!(result["messages"][2]["role"], "tool");
}
#[test]
fn chat_flushes_when_transcript_advances() {
    let payload = json!({"messages":[assistant(&["a"]),{"role":"user","content":"hello"}]});
    let result = chat(&payload);
    assert_eq!(result["messages"][1]["tool_call_id"], "a");
    assert_eq!(result["messages"][2]["role"], "user");
}
#[test]
fn chat_removes_duplicates_when_result_repeated() {
    let payload = json!({"messages":[assistant(&["a"]),output("a"),output("a")]});
    assert_eq!(chat(&payload)["messages"], json!([assistant(&["a"]),output("a")]));
}
#[test]
fn chat_removes_invalid_ids_when_missing_or_empty() {
    let payload = json!({"messages":[assistant(&["a"]),{"role":"tool","content":"missing"},output(""),output("a")]});
    assert_eq!(chat(&payload)["messages"], json!([assistant(&["a"]),output("a")]));
}
#[test]
fn chat_preserves_input_when_changed() {
    let payload = json!({"messages":[assistant(&["a"]),output("a"),output("orphan")]});
    let before = payload.clone();
    let result = chat(&payload);
    assert_ne!(result, before);
    assert_eq!(payload, before);
}
#[test]
fn responses_without_input_when_chat_payload() {
    let payload = json!({"messages":[]});
    assert_eq!(responses(&payload), payload);
}
#[test]
fn responses_orphan_function_when_missing_call() {
    let payload = json!({"input":[{"role":"user","content":[]},{"type":"function_call_output","call_id":"missing","output":"stale"}]});
    assert_eq!(responses(&payload)["input"], json!([{"role":"user","content":[]}]));
    assert_eq!(payload["input"].as_array().unwrap().len(), 2);
}
#[test]
fn responses_orphan_custom_when_missing_call() {
    let payload = json!({"input":[{"type":"custom_tool_call_output","call_id":"missing","output":"stale"}]});
    assert_eq!(responses(&payload)["input"], json!([]));
}
#[test]
fn responses_synthetic_function_when_missing_output() {
    let payload = json!({"input":[{"type":"function_call","call_id":"a","name":"bash","arguments":"{}"}]});
    let result = responses(&payload);
    assert_eq!(result["input"][1]["type"], "function_call_output");
    assert_eq!(result["input"][1]["call_id"], "a");
}
#[test]
fn responses_synthetic_custom_when_missing_output() {
    let payload = json!({"input":[{"type":"custom_tool_call","call_id":"a","name":"apply_patch"}]});
    let result = responses(&payload);
    assert_eq!(result["input"][1]["type"], "custom_tool_call_output");
    assert_eq!(result["input"][1]["name"], "apply_patch");
}
#[test]
fn responses_valid_pairs_when_complete() {
    let payload = json!({"input":[{"type":"function_call","call_id":"a"},{"type":"function_call_output","call_id":"a","output":"ok"},{"type":"custom_tool_call","call_id":"b"},{"type":"custom_tool_call_output","call_id":"b","output":"ok"}]});
    assert_eq!(responses(&payload), payload);
}
#[test]
fn responses_continuation_when_output_only_delta() {
    let payload = json!({"previous_response_id":"resp_1","input":[{"type":"function_call_output","call_id":"a","output":"ok"}]});
    assert_eq!(responses(&payload), payload);
}
