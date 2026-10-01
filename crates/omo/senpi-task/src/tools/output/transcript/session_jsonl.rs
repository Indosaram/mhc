//! Port of `tools/output/transcript/session-jsonl.ts`.

use serde_json::{Map, Value};

use crate::tools::output::types::TranscriptEntry;

/// Parse a senpi child-session JSONL transcript (docs/session-format.md) into assistant text entries.
/// The header line (`type:"session"`, v1/v2/v3) carries no message and is skipped like every other
/// non-assistant entry. Unknown entry types and malformed lines are tolerated so a future session
/// version never crashes task_output.
pub fn parse_session_transcript(text: &str) -> Vec<TranscriptEntry> {
    text.split('\n')
        .filter_map(|line| assistant_text_of(parse_line(line).as_ref()))
        .map(|text| TranscriptEntry::Assistant { text })
        .collect()
}

fn parse_line(line: &str) -> Option<Value> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

fn as_record(value: Option<&Value>) -> Option<&Map<String, Value>> {
    value?.as_object()
}

fn assistant_text_of(entry: Option<&Value>) -> Option<String> {
    let entry = as_record(entry)?;
    if entry.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let message = as_record(entry.get("message"))?;
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let content = message.get("content")?.as_array()?;
    let text: String = content.iter().filter_map(text_part_of).collect();
    if text.is_empty() { None } else { Some(text) }
}

fn text_part_of(part: &Value) -> Option<&str> {
    let part = part.as_object()?;
    if part.get("type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    part.get("text")?.as_str()
}
