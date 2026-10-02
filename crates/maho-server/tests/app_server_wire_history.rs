use maho_server::app_server::wire_thread::{logged_turn_with_view,turns_from_session_entries};
use serde_json::json;

#[test]
fn persisted_messages_reconstruct_turns_and_summary_views() {
    let entries = vec![json!({"type":"message","id":"u","timestamp":"2020-01-01T00:00:00.000Z","message":{"role":"user","content":"hello"}}),json!({"type":"message","id":"a1","message":{"role":"assistant","content":[{"type":"text","text":"first"}]}}),json!({"type":"message","id":"a2","message":{"role":"assistant","content":[{"type":"text","text":"last"}]}})];
    let turns = turns_from_session_entries(&entries,"fallback");
    assert_eq!(turns.len(),1);
    assert_eq!(turns[0].turn_id,"turn-1");
    assert_eq!(logged_turn_with_view(&turns[0],"full")["items"].as_array().unwrap().len(),3);
    let summary = logged_turn_with_view(&turns[0],"summary");
    assert_eq!(summary["items"][0]["id"],"u");
    assert_eq!(summary["items"][1]["id"],"a2");
    assert!(logged_turn_with_view(&turns[0],"notLoaded")["items"].as_array().unwrap().is_empty());
}
