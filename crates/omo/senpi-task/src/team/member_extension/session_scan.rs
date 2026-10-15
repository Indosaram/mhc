use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::time::SystemTime;

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

    let needle = format!("messageId=\"{message_id}\"");
    let mut buf: Vec<u8> = Vec::new();
    for entry in &entries {
        let mut reader = BufReader::new(fs::File::open(session_dir.join(entry))?);
        loop {
            buf.clear();
            if reader.read_until(b'\n', &mut buf)? == 0 {
                break;
            }
            let raw = buf.strip_suffix(b"\n").unwrap_or(buf.as_slice());
            let line = String::from_utf8_lossy(raw);
            // The marker embeds the id verbatim, so lines without it can never match.
            if !line.contains(message_id) {
                continue;
            }
            if let Some(value) = parse_json_line(&line)
                && contains_envelope_marker(&value, &needle)
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Sorted `(name, len, mtime)` of every `*.jsonl` in the session dir. Transcripts are
/// append-only, so an unchanged signature means a previous miss is still a miss.
pub type SessionJsonlSignature = Vec<(String, u64, Option<SystemTime>)>;

pub fn session_jsonl_signature(session_dir: &Path) -> io::Result<SessionJsonlSignature> {
    let read_dir = match fs::read_dir(session_dir) {
        Ok(read_dir) => read_dir,
        Err(error) if is_missing_path(&error) => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut signature: SessionJsonlSignature = Vec::new();
    for entry in read_dir {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".jsonl") {
            let metadata = entry.metadata()?;
            signature.push((name, metadata.len(), metadata.modified().ok()));
        }
    }
    signature.sort();
    Ok(signature)
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

fn contains_envelope_marker(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(text) => text.contains("<peer_message ") && text.contains(needle),
        Value::Array(items) => items
            .iter()
            .any(|entry| contains_envelope_marker(entry, needle)),
        Value::Object(map) => map
            .values()
            .any(|entry| contains_envelope_marker(entry, needle)),
        _ => false,
    }
}
