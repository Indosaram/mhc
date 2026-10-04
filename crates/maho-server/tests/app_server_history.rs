use maho_server::app_server::history::*;
use serde_json::json;
#[test]
fn persisted_history_contains_only_user_messages_and_preserves_image_inputs() {
    let branch = vec![json!({"type":"message","id":"u","message":{"role":"user","content":[{"type":"text","text":"hello"},{"type":"image","mimeType":"image/png","data":"AQ=="}]}}), json!({"type":"message","id":"a","message":{"role":"assistant","content":[]}}), json!({"type":"custom","id":"c"})];
    let turns = persisted_history_turns(&branch, "thread");
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["id"], "history-thread-u");
    assert_eq!(turns[0]["items"][0]["content"][1], json!({"type":"image","url":"data:image/png;base64,AQ=="}));
    assert_eq!(user_input_from_message(&json!({"content":"text"})), vec![json!({"type":"text","text":"text","text_elements":[]})]);
}
