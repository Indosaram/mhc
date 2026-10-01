//! `team/member-extension/session-scan.test.ts`
use std::fs;

use crate::team::member_extension::session_scan::session_jsonl_contains_message;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn given_a_hidden_custom_message_with_a_peer_envelope_when_scanned_then_it_finds_the_message_id() {
    let root = tempfile::tempdir().unwrap();
    let message_id = "message-hidden-1";
    let content = format!(
        "<peer_message from=\"lead\" to=\"worker\" messageId=\"{message_id}\">hello</peer_message>"
    );
    let line = json!({
        "type": "custom_message",
        "customType": "senpi-task:team-message",
        "display": false,
        "content": content,
    });
    fs::write(root.path().join("session.jsonl"), format!("{line}\n")).unwrap();

    assert_eq!(
        session_jsonl_contains_message(root.path(), message_id).unwrap(),
        true
    );
}
