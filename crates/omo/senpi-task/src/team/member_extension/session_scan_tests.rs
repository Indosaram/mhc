//! `team/member-extension/session-scan.test.ts`
use std::fs;

use crate::team::member_extension::session_scan::{SessionAckScan, session_jsonl_contains_message};
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

#[test]
fn given_appended_and_rewritten_sessions_when_ack_scan_refreshes_then_it_matches_the_full_scan() {
    use std::io::Write;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("session.jsonl");
    let envelope = |id: &str| {
        json!({"content": format!("<peer_message from=\"lead\" messageId=\"{id}\">hi</peer_message>")})
            .to_string()
    };
    let mut scan = SessionAckScan::default();
    scan.refresh(root.path()).unwrap();
    assert_eq!(scan.contains("m-1"), false);

    let line = envelope("m-1");
    let (head, tail) = line.split_at(10);
    fs::write(&path, format!("{}\n{head}", json!({"type": "noise"}))).unwrap();
    scan.refresh(root.path()).unwrap();
    assert_eq!(scan.contains("m-1"), false);

    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    write!(file, "{tail}").unwrap();
    scan.refresh(root.path()).unwrap();
    assert_eq!(scan.contains("m-1"), session_jsonl_contains_message(root.path(), "m-1").unwrap());
    assert_eq!(scan.contains("m-1"), true);

    writeln!(file, "\n{}", envelope("m-2")).unwrap();
    scan.refresh(root.path()).unwrap();
    assert_eq!((scan.contains("m-1"), scan.contains("m-2"), scan.contains("m")), (true, true, false));

    fs::write(&path, format!("{}\n", envelope("m-3"))).unwrap();
    scan.refresh(root.path()).unwrap();
    assert_eq!((scan.contains("m-1"), scan.contains("m-3")), (false, true));
}
