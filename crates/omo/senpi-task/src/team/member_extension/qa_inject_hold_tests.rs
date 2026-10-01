//! `team/member-extension/qa-inject-hold.test.ts`
use std::collections::HashMap;
use std::fs;

use crate::team::member_extension::qa_inject_hold::{
    QA_HOLD_AFTER_INJECT_ENV, create_qa_after_inject_hold,
};
use pretty_assertions::assert_eq;
use serde_json::json;
use team_core::types::Message;

fn message() -> Message {
    Message::safe_parse(&json!({
        "version": 1,
        "messageId": "22222222-2222-4222-8222-222222222222",
        "from": "lead",
        "to": "member",
        "kind": "message",
        "body": "payload",
        "timestamp": 1,
    }))
    .expect("valid message")
}

#[test]
fn given_a_hold_path_outside_qa_when_resolved_then_the_test_hook_stays_disabled() {
    let env = HashMap::from([(QA_HOLD_AFTER_INJECT_ENV.to_string(), "/tmp/hold".to_string())]);

    let hook = create_qa_after_inject_hold(&env);

    assert_eq!(hook.is_none(), true);
}

#[test]
fn given_a_qa_release_file_when_an_injected_message_enters_the_hook_then_a_marker_is_written_before_release()
 {
    let root = tempfile::tempdir().unwrap();
    let hold_dir = root.path().join("hold");
    let marker_path = hold_dir.join("entered.json");
    fs::create_dir_all(&hold_dir).unwrap();
    fs::write(hold_dir.join("entered.json.release"), "release\n").unwrap();
    let env = HashMap::from([
        ("OMO_SENPI_QA".to_string(), "1".to_string()),
        (
            QA_HOLD_AFTER_INJECT_ENV.to_string(),
            marker_path.to_string_lossy().into_owned(),
        ),
    ]);
    let hook = create_qa_after_inject_hold(&env).expect("expected QA hold hook");

    hook(&message()).unwrap();

    let content = fs::read_to_string(&marker_path).unwrap();
    assert_eq!(content.contains("22222222-2222-4222-8222-222222222222"), true);
}
