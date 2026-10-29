use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

type Collected = (Vec<Value>, Vec<String>);

fn decode(chunks: &[&str]) -> Collected {
    let mut messages = Vec::new();
    let mut errors = Vec::new();
    {
        let mut decoder = LineDecoder::new(
            |value| messages.push(value),
            |raw: &str, _error: &serde_json::Error| errors.push(raw.to_string()),
        );
        for chunk in chunks {
            decoder.push(chunk.as_bytes());
        }
    }
    (messages, errors)
}

#[test]
fn encode_json_line_appends_a_newline() {
    assert_eq!(encode_json_line(&json!({"a": 1})), "{\"a\":1}\n");
}

#[test]
fn chunks_split_mid_line_are_reassembled() {
    assert_eq!(
        decode(&["{\"a\":", "1}\n{\"b\"", ":2}\n"]),
        (vec![json!({"a": 1}), json!({"b": 2})], Vec::new())
    );
}

#[test]
fn two_messages_in_one_chunk_are_both_emitted() {
    assert_eq!(
        decode(&["{\"a\":1}\n{\"b\":2}\n"]),
        (vec![json!({"a": 1}), json!({"b": 2})], Vec::new())
    );
}

#[test]
fn malformed_line_reports_parse_error_and_keeps_going() {
    assert_eq!(
        decode(&["nope\n{\"ok\":true}\n"]),
        (vec![json!({"ok": true})], vec!["nope".to_string()])
    );
}

#[test]
fn blank_lines_are_ignored() {
    assert_eq!(
        decode(&["\n  \n{\"a\":1}\n"]),
        (vec![json!({"a": 1})], Vec::new())
    );
}
