//! `tools/output/integration.test.ts`

use std::fs;
use std::io::Write;
use std::path::Path;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::manager::transcript_log::{TRANSCRIPT_ASSISTANT_EVENT, TRANSCRIPT_TOOL_EVENT};
use crate::state::TaskStatus;
use crate::tools::output::records_fakes::{RecordOverrides, make_record};
use crate::tools::output::render::{RenderOptions, render_transcript};
use crate::tools::output::transcript::event_log::read_event_log_transcript;
use crate::tools::output::transcript::reader::default_transcript_reader;
use crate::tools::output::transcript::session_dir::{child_session_dir, read_session_dir_transcript};
use crate::tools::output::types::{TranscriptEntry, TranscriptMode, TranscriptReaderInput};

/// Appends one event line to OUR event log (logs/<taskId>.jsonl), the same layout the store writes.
fn append_event(state_dir: &str, task_id: &str, event_type: &str, payload: Value) {
    let logs = Path::new(state_dir).join("logs");
    fs::create_dir_all(&logs).expect("create logs dir");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join(format!("{task_id}.jsonl")))
        .expect("open event log");
    let line = json!({
        "type": event_type,
        "payload": payload,
        "timestamp": "2024-12-03T14:00:00.000Z",
    });
    writeln!(file, "{line}").expect("write event line");
}

fn write_session_file(state_dir: &str, task_id: &str, file_name: &str, contents: &str) {
    let session_dir = child_session_dir(state_dir, task_id);
    fs::create_dir_all(&session_dir).expect("create session dir");
    fs::write(Path::new(&session_dir).join(file_name), contents).expect("write session jsonl");
}

fn state_dir_of(dir: &tempfile::TempDir) -> String {
    dir.path().to_str().expect("utf8 temp path").to_string()
}

#[test]
fn given_a_committed_team_message_waited_event_when_task_output_reads_the_event_log_then_the_recovered_body_is_visible()
{
    // given
    let dir = tempfile::tempdir().unwrap();
    let state_dir = state_dir_of(&dir);
    let record = make_record(RecordOverrides {
        task_id: Some("st_000000ef".to_string()),
        status: Some(TaskStatus::Completed),
        ..RecordOverrides::default()
    });
    append_event(
        &state_dir,
        &record.task_id,
        "team_message_waited",
        json!({ "message_id": "message-1", "from": "alpha", "body": "recovered after lead restart" }),
    );

    // when
    let read = default_transcript_reader(&TranscriptReaderInput {
        task_id: &record.task_id,
        state_dir: &state_dir,
    })
    .unwrap();
    let rendered = render_transcript(
        &read.entries,
        &RenderOptions {
            mode: TranscriptMode::Full,
            tail_lines: 60,
        },
    );

    // then
    assert!(
        rendered
            .text
            .contains("[team message from alpha] recovered after lead restart"),
        "transcript: {}",
        rendered.text
    );
    assert_eq!(read.source.as_str(), "event-log");
}

#[test]
fn given_a_store_event_log_with_transcript_events_when_read_then_assistant_and_tool_entries_are_reconstructed_in_order()
{
    // given
    let dir = tempfile::tempdir().unwrap();
    let state_dir = state_dir_of(&dir);
    append_event(&state_dir, "st_000000ee", "reconcile_lost", json!({ "reason": "noise" }));
    append_event(&state_dir, "st_000000ee", TRANSCRIPT_ASSISTANT_EVENT, json!({ "text": "hello world" }));
    append_event(
        &state_dir,
        "st_000000ee",
        TRANSCRIPT_TOOL_EVENT,
        json!({ "tool": "bash", "is_error": false }),
    );
    append_event(&state_dir, "st_000000ee", TRANSCRIPT_ASSISTANT_EVENT, json!({ "text": "goodbye" }));

    // when
    let entries = read_event_log_transcript(&state_dir, "st_000000ee").unwrap();

    // then
    assert_eq!(
        entries,
        vec![
            TranscriptEntry::Assistant {
                text: "hello world".to_string()
            },
            TranscriptEntry::Tool {
                tool: "bash".to_string(),
                is_error: false
            },
            TranscriptEntry::Assistant {
                text: "goodbye".to_string()
            },
        ]
    );
}

#[test]
fn given_no_log_file_when_read_then_an_empty_list_is_returned_without_throwing() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let state_dir = state_dir_of(&dir);

    // when
    let entries = read_event_log_transcript(&state_dir, "st_00m1ss1n").unwrap();

    // then
    assert!(entries.is_empty());
}

#[test]
fn given_a_child_session_jsonl_on_disk_when_read_then_assistant_text_entries_are_extracted() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let state_dir = state_dir_of(&dir);
    let jsonl = [
        r#"{"type":"session","version":3,"id":"s"}"#,
        r#"{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"from session file"}]}}"#,
    ]
    .join("\n");
    write_session_file(&state_dir, "st_0000005e", "2024_abc.jsonl", &jsonl);

    // when
    let entries = read_session_dir_transcript(&state_dir, "st_0000005e").unwrap();

    // then
    assert_eq!(
        entries,
        vec![TranscriptEntry::Assistant {
            text: "from session file".to_string()
        }]
    );
}

#[test]
fn given_both_sources_present_when_read_then_the_event_log_wins_and_the_source_is_reported() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let state_dir = state_dir_of(&dir);
    append_event(&state_dir, "st_000000bb", TRANSCRIPT_ASSISTANT_EVENT, json!({ "text": "event log wins" }));
    write_session_file(
        &state_dir,
        "st_000000bb",
        "s.jsonl",
        r#"{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"session loses"}]}}"#,
    );

    // when
    let result = default_transcript_reader(&TranscriptReaderInput {
        task_id: "st_000000bb",
        state_dir: &state_dir,
    })
    .unwrap();

    // then
    assert_eq!(result.source.as_str(), "event-log");
    assert_eq!(
        result.entries,
        vec![TranscriptEntry::Assistant {
            text: "event log wins".to_string()
        }]
    );
}

#[test]
fn given_only_a_session_dir_when_read_then_the_session_jsonl_source_is_used() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let state_dir = state_dir_of(&dir);
    write_session_file(
        &state_dir,
        "st_0000001a",
        "s.jsonl",
        r#"{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"only session"}]}}"#,
    );

    // when
    let result = default_transcript_reader(&TranscriptReaderInput {
        task_id: "st_0000001a",
        state_dir: &state_dir,
    })
    .unwrap();

    // then
    assert_eq!(result.source.as_str(), "session-jsonl");
    assert_eq!(
        result.entries,
        vec![TranscriptEntry::Assistant {
            text: "only session".to_string()
        }]
    );
}

#[test]
fn given_neither_source_when_read_then_the_source_is_none() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let state_dir = state_dir_of(&dir);

    // when
    let result = default_transcript_reader(&TranscriptReaderInput {
        task_id: "st_00000ffe",
        state_dir: &state_dir,
    })
    .unwrap();

    // then
    assert_eq!(result.source.as_str(), "none");
    assert!(result.entries.is_empty());
}
