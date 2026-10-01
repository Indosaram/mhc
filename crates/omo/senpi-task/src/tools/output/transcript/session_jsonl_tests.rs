//! `tools/output/transcript/session-jsonl.test.ts`

use pretty_assertions::assert_eq;

use crate::tools::output::transcript::session_jsonl::parse_session_transcript;
use crate::tools::output::types::TranscriptEntry;

// Inline equivalents of test-support/session-jsonl/{v1,v2,v3}.jsonl so the tests are hermetic.
const V1_JSONL: &str = concat!(
    r#"{"type":"session","id":"sess-v1","timestamp":"2024-01-01T00:00:00.000Z","cwd":"/work"}"#,
    "\n",
    r#"{"type":"message","timestamp":"2024-01-01T00:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"Why does the build fail?"}]}}"#,
    "\n",
    r#"{"type":"message","timestamp":"2024-01-01T00:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"The build fails because of a missing import."}]}}"#,
    "\n",
    r#"{"type":"message","timestamp":"2024-01-01T00:00:03.000Z","message":{"role":"user","content":[{"type":"text","text":"Fix it."}]}}"#,
    "\n",
    r#"{"type":"message","timestamp":"2024-01-01T00:00:04.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Fixed it. The import path was wrong."}]}}"#,
    "\n",
);

const V2_JSONL: &str = concat!(
    r#"{"type":"session","version":2,"id":"sess-v2","timestamp":"2024-01-01T00:00:00.000Z","cwd":"/work"}"#,
    "\n",
    r#"{"type":"model_change","id":"e1","parentId":null,"timestamp":"2024-01-01T00:00:00.500Z","provider":"anthropic","modelId":"claude"}"#,
    "\n",
    r#"{"type":"message","id":"e2","parentId":"e1","timestamp":"2024-01-01T00:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"What does the module do?"}]}}"#,
    "\n",
    r#"{"type":"message","id":"e3","parentId":"e2","timestamp":"2024-01-01T00:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"The module exposes a task manager."},{"type":"toolCall","id":"call-1","name":"read","arguments":{"path":"src/index.ts"}}]}}"#,
    "\n",
    r#"{"type":"message","id":"e4","parentId":"e3","timestamp":"2024-01-01T00:00:03.000Z","message":{"role":"toolResult","toolCallId":"call-1","toolName":"read","content":[{"type":"text","text":"export class RecordStore {}"}]}}"#,
    "\n",
    r#"{"type":"message","id":"e5","parentId":"e4","timestamp":"2024-01-01T00:00:04.000Z","message":{"role":"assistant","content":[{"type":"text","text":"It also owns the record store."}]}}"#,
    "\n",
);

const V3_JSONL: &str = concat!(
    r#"{"type":"session","version":3,"id":"sess-v3","timestamp":"2024-01-01T00:00:00.000Z","cwd":"/work"}"#,
    "\n",
    r#"{"type":"custom","id":"e1","parentId":null,"timestamp":"2024-01-01T00:00:00.500Z","customType":"ext-state","data":{"k":"v"}}"#,
    "\n",
    r#"{"type":"message","id":"e2","parentId":"e1","timestamp":"2024-01-01T00:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"Ship the release."}]}}"#,
    "\n",
    r#"{"type":"message","id":"e3","parentId":"e2","timestamp":"2024-01-01T00:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Cutting the release now."}]}}"#,
    "\n",
    r#"{"type":"custom_message","id":"e4","parentId":"e3","timestamp":"2024-01-01T00:00:03.000Z","customType":"notice","content":"CI started","display":true}"#,
    "\n",
    r#"{"type":"future_entry_kind","id":"e5","parentId":"e4","timestamp":"2024-01-01T00:00:03.500Z","payload":{"anything":[1,2,3]}}"#,
    "\n",
    r#"{"type":"message","id":"e6","parentId":"e5","timestamp":"2024-01-01T00:00:04.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Release v9 is live."}]}}"#,
    "\n",
);

fn fixture(name: &str) -> &'static str {
    match name {
        "v1.jsonl" => V1_JSONL,
        "v2.jsonl" => V2_JSONL,
        "v3.jsonl" => V3_JSONL,
        other => panic!("unknown fixture {other}"),
    }
}

#[allow(unreachable_patterns, irrefutable_let_patterns)]
fn assistant_texts(text: &str) -> Vec<String> {
    parse_session_transcript(text)
        .into_iter()
        .filter_map(|entry| match entry {
            TranscriptEntry::Assistant { text } => Some(text),
            _ => None,
        })
        .collect()
}

#[test]
fn given_v1_linear_session_when_parsed_then_only_assistant_text_entries_are_extracted_in_order() {
    // given
    let text = fixture("v1.jsonl");

    // when
    let texts = assistant_texts(text);

    // then
    assert_eq!(
        texts,
        vec![
            "The build fails because of a missing import.".to_string(),
            "Fixed it. The import path was wrong.".to_string(),
        ]
    );
}

#[test]
fn given_v2_tree_session_with_model_change_and_tool_result_when_parsed_then_user_tool_non_message_entries_are_skipped() {
    // given
    let text = fixture("v2.jsonl");

    // when
    let texts = assistant_texts(text);

    // then
    assert_eq!(
        texts,
        vec![
            "The module exposes a task manager.".to_string(),
            "It also owns the record store.".to_string(),
        ]
    );
}

#[test]
fn given_v3_session_with_custom_custom_message_and_unknown_future_entry_when_parsed_then_unknown_types_are_tolerated() {
    // given
    let text = fixture("v3.jsonl");

    // when
    let texts = assistant_texts(text);

    // then
    assert_eq!(
        texts,
        vec!["Cutting the release now.".to_string(), "Release v9 is live.".to_string()]
    );
}

#[test]
fn given_malformed_lines_mixed_with_valid_ones_when_parsed_then_broken_lines_are_skipped_without_throwing() {
    // given
    let text = [
        r#"{"type":"session","version":3,"id":"x"}"#,
        "not-json-at-all",
        r#"{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"survived"}]}}"#,
        "",
    ]
    .join("\n");

    // when
    let texts = assistant_texts(&text);

    // then
    assert_eq!(texts, vec!["survived".to_string()]);
}

#[test]
fn given_assistant_message_with_multiple_text_blocks_when_parsed_then_blocks_are_concatenated() {
    // given
    let text = r#"{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"part one "},{"type":"thinking","thinking":"hmm"},{"type":"text","text":"part two"}]}}"#;

    // when
    let texts = assistant_texts(text);

    // then
    assert_eq!(texts, vec!["part one part two".to_string()]);
}
