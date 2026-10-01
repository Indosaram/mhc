use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;

/// Detects whether a delivered peer_message envelope has landed in the member's session JSONL.
/// This is the delivery ack the self-poller uses before committing a reservation.
pub fn session_jsonl_contains_message(session_dir: &Path, message_id: &str) -> io::Result<bool> {
    let read_dir = match fs::read_dir(session_dir) {
        Ok(read_dir) => read_dir,
        Err(error) if is_missing_path(&error) => return Ok(false),
        Err(error) => return Err(error),
    };
    let mut entries: Vec<String> = Vec::new();
    for entry in read_dir {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if is_missing_path(&error) => return Ok(false),
            Err(error) => return Err(error),
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".jsonl") {
            entries.push(name);
        }
    }
    entries.sort();

    for entry in &entries {
        let bytes = fs::read(session_dir.join(entry))?;
        let text = String::from_utf8_lossy(&bytes);
        for line in text.split('\n') {
            if let Some(value) = parse_json_line(line)
                && contains_envelope_marker(&value, message_id)
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub fn is_missing_path(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
}

fn parse_json_line(line: &str) -> Option<Value> {
    if line.trim().is_empty() {
        return None;
    }
    serde_json::from_str(line).ok()
}

fn contains_envelope_marker(value: &Value, message_id: &str) -> bool {
    match value {
        Value::String(text) => {
            text.contains("<peer_message ") && text.contains(&format!("messageId=\"{message_id}\""))
        }
        Value::Array(items) => items
            .iter()
            .any(|entry| contains_envelope_marker(entry, message_id)),
        Value::Object(map) => map
            .values()
            .any(|entry| contains_envelope_marker(entry, message_id)),
        _ => false,
    }
}
