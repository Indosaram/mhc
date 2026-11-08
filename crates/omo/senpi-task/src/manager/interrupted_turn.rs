//! Detects a session transcript whose last turn was cut off (`manager/interrupted-turn.ts`).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

const INITIAL_TAIL_BYTES: u64 = 64 * 1024;

pub fn session_tail_needs_continuation(session_path: &Path) -> bool {
    let Some(line) = read_last_message_line(session_path) else {
        return false;
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&line) else {
        return false;
    };
    let Some(message) = parsed.get("message").and_then(Value::as_object) else {
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

/// The last line that carries a message. Session-lifecycle `custom` entries are appended after a
/// turn settles, so the literal last line is routinely NOT the turn's last message; judging the
/// tail by it silently reports a cut-off turn as clean.
fn read_last_message_line(session_path: &Path) -> Option<String> {
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
        // The first line of a window that does not start at byte 0 may be cut in half.
        let lines: Vec<&str> = text.split('\n').collect();
        let complete = usize::from(start > 0);
        for line in lines.iter().skip(complete).rev() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(parsed) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if parsed.get("type").and_then(Value::as_str) == Some("message") {
                return Some(line.to_string());
            }
        }
        if start == 0 {
            return None;
        }
        bytes = size.min(bytes.saturating_mul(2));
    }
}
