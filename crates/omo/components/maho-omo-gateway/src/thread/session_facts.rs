//! Bounded session JSONL fact reader (`thread/session-facts.ts`).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// How many bytes each end of a session file is read for its facts: listing never reads a whole transcript.
pub const SESSION_FACTS_WINDOW_BYTES: usize = 64 * 1024;

/// Largest final JSONL entry read to prove the newest timestamp; larger entries make activity unknown.
pub const SESSION_FACTS_LAST_LINE_MAX_BYTES: usize = SESSION_FACTS_WINDOW_BYTES * 4;

/// A thread with no name is shown by the start of its first user message, cut to this many characters.
pub const THREAD_TITLE_MAX_CHARS: usize = 60;

const SESSION_ARCHIVED_CUSTOM_TYPE: &str = "session_archived";

pub type JsonRecord = Map<String, Value>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFacts {
    pub durable_id: String,
    pub cwd: String,
    pub created_at: String,
    pub updated_at: Option<String>,
    /// The last `session_info` name (`/name`, `set_session_name`), or `None` when none was set or cleared.
    pub name: Option<String>,
    pub first_user_text: Option<String>,
    /// The newest `session_archived` entry archived the thread.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
}

pub fn message_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            let mut out = Vec::new();
            for part in items {
                if let Some(obj) = part.as_object()
                    && obj.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(text) = obj.get("text").and_then(Value::as_str)
                {
                    out.push(text);
                }
            }
            out.join(" ")
        }
        _ => String::new(),
    }
}

/// The display title of a thread: its name, else the first user message's opening; never its id.
pub fn thread_title(name: Option<&str>, first_user_text: Option<&str>) -> Option<String> {
    if let Some(explicit) = name.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(explicit.to_owned());
    }
    let raw = first_user_text?;
    let collapsed: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.chars().take(THREAD_TITLE_MAX_CHARS).collect())
}

/// Each line of a session JSONL that parses as an object; `drop_first`/`drop_last` discard cut window boundaries.
pub fn parse_session_lines(text: &str, drop_first: bool, drop_last: bool) -> Vec<JsonRecord> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if drop_first && !lines.is_empty() {
        lines.remove(0);
    }
    if drop_last && !lines.is_empty() {
        lines.pop();
    }
    let mut out = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(trimmed) {
            out.push(map);
        }
    }
    out
}

#[derive(Debug, Clone, Default)]
pub struct SessionEntrySummary {
    pub header: Option<JsonRecord>,
    pub name: Option<String>,
    pub first_user_text: Option<String>,
    pub newest_timestamp: String,
    pub archived: bool,
}

/// One pass over session entries in file order.
pub fn summarize_session_entries(entries: &[JsonRecord]) -> SessionEntrySummary {
    let mut header: Option<JsonRecord> = None;
    let mut name: Option<String> = None;
    let mut first_user_text: Option<String> = None;
    let mut newest = String::new();
    let mut archived = false;

    for entry in entries {
        let entry_type = entry.get("type").and_then(Value::as_str);
        if entry_type == Some("custom")
            && entry.get("customType").and_then(Value::as_str) == Some(SESSION_ARCHIVED_CUSTOM_TYPE)
        {
            archived = entry
                .get("data")
                .and_then(Value::as_object)
                .and_then(|d| d.get("archived"))
                .and_then(Value::as_bool)
                == Some(true);
        }
        if header.is_none()
            && entry_type == Some("session")
            && entry.get("id").and_then(Value::as_str).is_some()
        {
            header = Some(entry.clone());
        }
        if entry_type == Some("session_info") {
            name = entry
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
        }
        if first_user_text.is_none()
            && entry_type == Some("message")
            && let Some(msg) = entry.get("message").and_then(Value::as_object)
            && msg.get("role").and_then(Value::as_str) == Some("user")
            && let Some(content) = msg.get("content")
        {
            let text = message_text(content).trim().to_owned();
            if !text.is_empty() {
                first_user_text = Some(text);
            }
        }
        if let Some(ts) = entry.get("timestamp").and_then(Value::as_str)
            && ts > newest.as_str()
        {
            newest = ts.to_owned();
        }
    }

    SessionEntrySummary {
        header,
        name,
        first_user_text,
        newest_timestamp: newest,
        archived,
    }
}

fn final_entry_timestamp(line: Option<&[u8]>) -> Option<String> {
    let bytes = line?;
    let text = std::str::from_utf8(bytes).ok()?;
    let val: Value = serde_json::from_str(text).ok()?;
    val.as_object()?
        .get("timestamp")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn read_at(file: &mut File, pos: u64, len: usize) -> Option<Vec<u8>> {
    file.seek(SeekFrom::Start(pos)).ok()?;
    let mut buf = vec![0u8; len];
    let n = file.read(&mut buf).ok()?;
    buf.truncate(n);
    Some(buf)
}

fn final_complete_line(file: &mut File, size: u64) -> Option<Vec<u8>> {
    if size == 0 {
        return None;
    }
    let last = read_at(file, size - 1, 1)?;
    if last.first().copied() != Some(b'\n') {
        return None;
    }
    let mut remaining = (size - 1) as usize;
    let mut scanned = 0usize;
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    while remaining > 0 && scanned < SESSION_FACTS_LAST_LINE_MAX_BYTES {
        let len = SESSION_FACTS_WINDOW_BYTES
            .min(remaining)
            .min(SESSION_FACTS_LAST_LINE_MAX_BYTES - scanned);
        let start = remaining - len;
        let chunk = read_at(file, start as u64, len)?;
        if chunk.len() != len {
            return None;
        }
        if let Some(nl) = chunk.iter().rposition(|b| *b == b'\n') {
            chunks.push(chunk[nl + 1..].to_vec());
            chunks.reverse();
            return Some(chunks.concat());
        }
        chunks.push(chunk);
        if start == 0 {
            chunks.reverse();
            return Some(chunks.concat());
        }
        scanned += len;
        remaining = start;
    }
    None
}

fn mtime_ns(meta: &std::fs::Metadata) -> Option<u128> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
}

/// Bounded reads of one session file (`readSessionFacts`). Returns `None` if unreadable or no session header.
pub fn read_session_facts(path: &Path) -> Option<SessionFacts> {
    let mut file = File::open(path).ok()?;
    let before = file.metadata().ok()?;
    let size = before.len() as usize;
    let head_len = size.min(SESSION_FACTS_WINDOW_BYTES);
    let head = read_at(&mut file, 0, head_len)?;
    let whole = size <= SESSION_FACTS_WINDOW_BYTES;
    let head_text = String::from_utf8_lossy(&head);
    let head_entries = parse_session_lines(&head_text, false, !whole);
    let first = head_entries.first()?;
    if first.get("type").and_then(Value::as_str) != Some("session") {
        return None;
    }
    let durable_id = first.get("id").and_then(Value::as_str)?.to_owned();
    let cwd = first.get("cwd").and_then(Value::as_str)?.to_owned();
    let created_at = first.get("timestamp").and_then(Value::as_str)?.to_owned();

    let mut all_entries = head_entries;
    if !whole {
        let tail_start = head_len.max(size - SESSION_FACTS_WINDOW_BYTES);
        let tail = read_at(&mut file, (tail_start - 1) as u64, size - tail_start + 1)?;
        if let Some((&first_byte, rest)) = tail.split_first() {
            let tail_text = String::from_utf8_lossy(rest);
            let tail_entries = parse_session_lines(&tail_text, first_byte != b'\n', false);
            all_entries.extend(tail_entries);
        }
    }

    let summary = summarize_session_entries(&all_entries);
    let final_line = final_complete_line(&mut file, before.len());
    let after = file.metadata().ok()?;
    let unchanged = after.len() == before.len() && mtime_ns(&after) == mtime_ns(&before);
    let updated_at = if unchanged {
        final_entry_timestamp(final_line.as_deref())
    } else {
        None
    };

    Some(SessionFacts {
        durable_id,
        cwd,
        created_at,
        updated_at,
        name: summary.name,
        first_user_text: summary.first_user_text,
        archived: summary.archived,
    })
}

pub async fn read_session_facts_async(path: &Path) -> Option<SessionFacts> {
    let owned = path.to_path_buf();
    tokio::task::spawn_blocking(move || read_session_facts(&owned))
        .await
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_header_name_first_user_archived_and_updated_at() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sess.jsonl");
        let lines = [
            serde_json::json!({ "type": "session", "id": "sess-1", "cwd": "/repo", "timestamp": "2026-01-01T00:00:00Z" }),
            serde_json::json!({ "type": "session_info", "name": "Build runner", "timestamp": "2026-01-01T00:00:01Z" }),
            serde_json::json!({ "type": "message", "message": { "role": "user", "content": [{ "type": "text", "text": "Fix tests" }] }, "timestamp": "2026-01-01T00:00:02Z" }),
            serde_json::json!({ "type": "custom", "customType": "session_archived", "data": { "archived": true }, "timestamp": "2026-01-01T00:00:05Z" }),
        ];
        let body: String = lines.iter().map(|v| format!("{v}\n")).collect();
        std::fs::write(&file, body).unwrap();
        let facts = read_session_facts(&file).unwrap();
        assert_eq!(facts.durable_id, "sess-1");
        assert_eq!(facts.name.as_deref(), Some("Build runner"));
        assert_eq!(facts.first_user_text.as_deref(), Some("Fix tests"));
        assert!(facts.archived);
        assert_eq!(facts.updated_at.as_deref(), Some("2026-01-01T00:00:05Z"));
    }
}
