//! Port of `tools/output/transcript/event-log.ts`.

use std::io;
use std::path::Path;

use serde_json::{Map, Value};

use crate::tools::output::transcript::read_bounded::read_bounded_file_text;
use crate::tools::output::types::{TranscriptEntry, TranscriptReadResult, TranscriptSource};

// Re-exported from the writer (manager/transcript_log.rs) so reader and writer share ONE contract for
// the event-type names and can never drift.
pub use crate::manager::transcript_log::{TRANSCRIPT_ASSISTANT_EVENT, TRANSCRIPT_ERROR_EVENT, TRANSCRIPT_TOOL_EVENT};

/// Reconstruct a child's transcript from OUR event log (logs/<taskId>.jsonl). Only the transcript
/// event types are lifted; lifecycle/audit events on the same log are ignored. A missing log is an
/// empty transcript, never an error (task_output is read-only and must tolerate absent state).
pub fn read_event_log_transcript(state_dir: &str, task_id: &str) -> io::Result<Vec<TranscriptEntry>> {
    Ok(read_event_log_transcript_result(state_dir, task_id)?.entries)
}

pub fn read_event_log_transcript_result(state_dir: &str, task_id: &str) -> io::Result<TranscriptReadResult> {
    let path = Path::new(state_dir).join("logs").join(format!("{task_id}.jsonl"));
    let Some(raw) = read_bounded_file_text(&path, None)? else {
        return Ok(TranscriptReadResult {
            entries: Vec::new(),
            source: TranscriptSource::EventLog,
            truncated: Some(false),
        });
    };
    let entries = raw
        .text
        .split('\n')
        .filter_map(|line| transcript_entry_of(parse_line(line).as_ref()))
        .collect();
    Ok(TranscriptReadResult {
        entries,
        source: TranscriptSource::EventLog,
        truncated: Some(raw.truncated),
    })
}

fn parse_line(line: &str) -> Option<Value> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

fn string_field<'a>(record: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    record.get(key).and_then(Value::as_str)
}

fn transcript_entry_of(entry: Option<&Value>) -> Option<TranscriptEntry> {
    let entry = entry?.as_object()?;
    let payload = entry.get("payload")?.as_object()?;
    let event_type = string_field(entry, "type");

    if event_type == Some(TRANSCRIPT_ASSISTANT_EVENT)
        && let Some(text) = string_field(payload, "text")
    {
        return Some(TranscriptEntry::Assistant { text: text.to_string() });
    }
    if event_type == Some(TRANSCRIPT_TOOL_EVENT)
        && let Some(tool) = string_field(payload, "tool")
    {
        return Some(TranscriptEntry::Tool {
            tool: tool.to_string(),
            is_error: payload.get("is_error") == Some(&Value::Bool(true)),
        });
    }
    if event_type == Some(TRANSCRIPT_ERROR_EVENT)
        && let Some(message) = string_field(payload, "message")
    {
        return Some(TranscriptEntry::Error {
            message: message.to_string(),
        });
    }
    if event_type == Some("team_message_waited")
        && let (Some(from), Some(body)) = (string_field(payload, "from"), string_field(payload, "body"))
    {
        return Some(TranscriptEntry::Assistant {
            text: format!("[team message from {from}] {body}"),
        });
    }
    None
}
