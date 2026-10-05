use maho_server::app_server::turn_runtime::{build_turn, build_user_message, parse_input, read_logged_items};
use maho_server::app_server::turn_log::{RecordTurnOptions, TurnLog};
use serde_json::json;

#[test]
fn text_input_preserves_content_and_joins_text() {
    let input = [json!({"type":"text","text":"first"}), json!({"type":"text","text":"second","text_elements":[{"start":0}]})];
    let parsed = parse_input(&input).unwrap();
    assert_eq!(parsed.text, "first\nsecond");
    assert_eq!(parsed.content[0]["text_elements"], json!([]));
    assert_eq!(parsed.content[1]["text_elements"], input[1]["text_elements"]);
}

#[test]
fn invalid_and_unsupported_input_rejects_at_the_boundary() {
    for input in [vec![], vec![json!({"type":"text","text":"  "})], vec![json!({"type":"image"})], vec![json!({"type":"localImage"})], vec![json!({"type":"skill"})], vec![json!({"type":"mention"})], vec![json!({"type":"unknown"})]] {
        assert_eq!(parse_input(&input).err().unwrap().code, -32602);
    }
}

#[test]
fn terminal_turns_use_seconds_and_nullable_completion_fields() {
    let running = build_turn("turn", "inProgress", 1000.0, None, &[], None);
    assert_eq!(running["startedAt"], 1.0);
    assert!(running["completedAt"].is_null());
    assert!(running["error"].is_null());
    let failed = build_turn("turn", "failed", 1000.0, Some(2500.0), &[], Some("failure"));
    assert_eq!(failed["durationMs"], 1500.0);
    assert_eq!(failed["error"]["message"], "failure");
}

#[test]
fn user_message_retains_client_identity_and_content() {
    let content = [json!({"type":"text","text":"hello","text_elements":[]})];
    let message = build_user_message(Some("client"), &content);
    assert_eq!(message["id"], "client");
    assert_eq!(message["clientId"], "client");
    assert_eq!(message["content"], json!(content));
    let generated = build_user_message(None, &content);
    assert!(uuid::Uuid::parse_str(generated["id"].as_str().unwrap()).is_ok());
    assert!(generated["clientId"].is_null());
}

#[test]
fn text_validation_uses_ecmascript_trim_without_altering_input() {
    assert_eq!(parse_input(&[json!({"type":"text","text":"\u{feff}"})]).err().unwrap().code,-32602);
    assert_eq!(parse_input(&[json!({"type":"text","text":"\u{0085}"})]).unwrap().text,"\u{0085}");
}

#[test]
fn logged_items_are_returned_as_json_for_the_matching_turn_only() {
    // Pinned `readLoggedItems` (turn-runtime.ts): the logged items of the named turn, converted to
    // JSON. Rust WireItem is already a serde_json::Map, so no `wireItemToJson` conversion is needed
    // (that export is N/A here); this proves the read side.
    let mut log = TurnLog::default();
    log.record_turn("thread", RecordTurnOptions { turn_id:"turn".into(), started_at:"2026-01-01T00:00:00.000Z".into(), status:None, completed_at:None, error:None });
    log.record_turn("thread", RecordTurnOptions { turn_id:"other".into(), started_at:"2026-01-01T00:00:00.000Z".into(), status:None, completed_at:None, error:None });
    log.append_item("thread","turn", json!({"type":"userMessage","id":"u"}).as_object().unwrap().clone()).unwrap();
    log.append_item("thread","other", json!({"type":"userMessage","id":"other"}).as_object().unwrap().clone()).unwrap();
    assert_eq!(read_logged_items(&mut log, "thread", "turn"), vec![json!({"type":"userMessage","id":"u"})]);
    assert!(read_logged_items(&mut log, "thread", "missing").is_empty());
}
