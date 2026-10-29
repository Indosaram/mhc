//! `tools/output/transcript/session-dir.test.ts`

use std::fs;
use std::path::Path;

use pretty_assertions::assert_eq;
use serde_json::json;

use crate::tools::output::transcript::session_dir::{child_session_dir, read_session_dir_transcript_result};

fn message(text: &str) -> String {
    format!(
        "{}\n",
        json!({
            "type": "message",
            "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] },
        })
    )
}

#[test]
fn given_more_than_two_child_session_files_when_read_then_only_bounded_edge_files_are_parsed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state_dir = temp.path().to_str().expect("utf8 temp path").to_string();
    let session_dir = child_session_dir(&state_dir, "st_1");
    fs::create_dir_all(&session_dir).expect("mkdir session dir");
    fs::write(Path::new(&session_dir).join("001.jsonl"), message("first")).expect("write 001");
    fs::write(Path::new(&session_dir).join("002.jsonl"), message("middle")).expect("write 002");
    fs::write(Path::new(&session_dir).join("003.jsonl"), message("last")).expect("write 003");

    let result = read_session_dir_transcript_result(&state_dir, "st_1").expect("read transcript");

    assert_eq!(result.truncated, Some(true));
    let entries = serde_json::to_value(&result.entries).expect("serialize entries");
    assert_eq!(
        entries,
        json!([
            { "kind": "assistant", "text": "first" },
            { "kind": "assistant", "text": "last" },
        ])
    );
}
