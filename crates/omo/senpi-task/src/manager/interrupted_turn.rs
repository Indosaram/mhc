//! Detects a session transcript whose last turn was cut off (`manager/interrupted-turn.ts`).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

const INITIAL_TAIL_BYTES: u64 = 64 * 1024;

pub fn session_tail_needs_continuation(session_path: &Path) -> bool {
    let Some(line) = read_last_jsonl_line(session_path) else {
        return false;
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&line) else {
        return false;
    };
    let Some(message) = parsed
        .as_object()
        .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("message"))
        .and_then(|entry| entry.get("message"))
        .and_then(Value::as_object)
    else {
        return false;
    };
    match message.get("role").and_then(Value::as_str) {
        Some("user" | "toolResult") => true,
        Some("assistant") => {
            message.get("stopReason").and_then(Value::as_str) == Some("aborted")
                || message
                    .get("content")
                    .and_then(Value::as_array)
                    .is_some_and(|content| {
                        content.iter().any(|part| {
                            part.as_object()
                                .and_then(|part| part.get("type"))
                                .and_then(Value::as_str)
                                == Some("toolCall")
                        })
                    })
        }
        _ => false,
    }
}

fn read_last_jsonl_line(session_path: &Path) -> Option<String> {
    let mut file = File::open(session_path).ok()?;
    let size = file.metadata().ok()?.len();
    if size == 0 {
        return None;
    }
    let mut bytes = size.min(INITIAL_TAIL_BYTES);
    loop {
        let start = size - bytes;
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut buffer = Vec::new();
        file.by_ref().take(bytes).read_to_end(&mut buffer).ok()?;
        let lossy = String::from_utf8_lossy(&buffer);
        let text = lossy.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            return None;
        }
        if let Some(delimiter) = text.rfind('\n') {
            let last = text[delimiter + 1..].trim();
            return (!last.is_empty()).then(|| last.to_string());
        }
        if start == 0 {
            let whole = text.trim();
            return (!whole.is_empty()).then(|| whole.to_string());
        }
        bytes = size.min(bytes * 2);
    }
}
