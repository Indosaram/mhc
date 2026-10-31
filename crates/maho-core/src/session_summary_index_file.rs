//! Port of senpi packages/coding-agent/src/core/session-summary-index-file.ts.
//!
//! The on-disk format of the summary index: a version header line, then one JSON line per indexed
//! session file. Last line wins; torn or foreign lines are skipped.

use std::collections::HashMap;
use std::io::{self, Write};

use serde_json::{Map, Value, json};

use crate::session_move::bundle::parse_repository_identity;
use crate::session_summary::SessionSummary;
use crate::session_summary_lru::FileStamp;

/// Name of the summary index inside the sessions directory it describes. The extension is deliberately
/// not `.jsonl`: every `*.jsonl` there is a session.
pub const SESSION_SUMMARY_INDEX_FILE: &str = ".session-summaries.index";

// 2: summaries carry the recorded repository identity; a version-1 index predates it and is re-read.
const INDEX_VERSION: u64 = 2;

fn header_line() -> String {
    format!("{}\n", json!({ "version": INDEX_VERSION }))
}

/// One indexed session file: its basename, the stamp it was read at, and its summary.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexEntry {
    pub file: String,
    pub stamp: FileStamp,
    pub summary: SessionSummary,
    /// Serialized line, newline included.
    pub line: String,
    pub line_bytes: usize,
}

/// An index as loaded from disk; an absent or unusable file loads as `usable: false`.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedIndex {
    pub usable: bool,
    /// Deduplicated entries, last line wins.
    pub entries: HashMap<String, IndexEntry>,
    /// Bytes after the header line, including torn and superseded lines.
    pub entry_bytes: usize,
    /// False when the file ends in a torn line, so the next append must start a new line.
    pub ends_with_newline: bool,
}

impl LoadedIndex {
    fn unusable() -> Self {
        Self {
            usable: false,
            entries: HashMap::new(),
            entry_bytes: 0,
            ends_with_newline: true,
        }
    }
}

pub(crate) fn summary_to_json(summary: &SessionSummary) -> Value {
    let mut object = Map::new();
    object.insert("header".to_owned(), summary.header.clone());
    if let Some(name) = &summary.name {
        object.insert("name".to_owned(), Value::from(name.as_str()));
    }
    object.insert(
        "firstUserMessage".to_owned(),
        Value::from(summary.first_user_message.as_str()),
    );
    object.insert(
        "messageCount".to_owned(),
        Value::from(summary.message_count),
    );
    if let Some(time) = summary.last_activity_time {
        object.insert("lastActivityTime".to_owned(), Value::from(time));
    }
    object.insert(
        "allMessagesText".to_owned(),
        Value::from(summary.all_messages_text.as_str()),
    );
    if let Some(identity) = &summary.repository_identity {
        object.insert("repositoryIdentity".to_owned(), identity.clone());
    }
    Value::Object(object)
}

fn optional<'a>(
    value: &'a Value,
    key: &str,
    check: fn(&Value) -> bool,
) -> Option<Option<&'a Value>> {
    match value.get(key) {
        None => Some(None),
        Some(field) if check(field) => Some(Some(field)),
        Some(_) => None,
    }
}

fn summary_from_json(value: &Value) -> Option<SessionSummary> {
    let header = value.get("header").filter(|header| header.is_object())?;
    if header.get("type").and_then(Value::as_str) != Some("session")
        || !header.get("id").is_some_and(Value::is_string)
    {
        return None;
    }
    let name = optional(value, "name", Value::is_string)?;
    let last_activity_time = optional(value, "lastActivityTime", Value::is_number)?;
    let repository_identity = match value.get("repositoryIdentity") {
        None => None,
        Some(identity) => {
            parse_repository_identity(identity)?;
            Some(identity.clone())
        }
    };
    Some(SessionSummary {
        header: header.clone(),
        name: name.and_then(Value::as_str).map(str::to_owned),
        first_user_message: value.get("firstUserMessage")?.as_str()?.to_owned(),
        message_count: usize::try_from(value.get("messageCount")?.as_u64()?).ok()?,
        last_activity_time: match last_activity_time {
            None => None,
            Some(time) => Some(
                time.as_i64()
                    .or_else(|| time.as_f64().map(|time| time as i64))?,
            ),
        },
        all_messages_text: value.get("allMessagesText")?.as_str()?.to_owned(),
        repository_identity,
    })
}

fn with_line(file: String, stamp: FileStamp, summary: SessionSummary, line: String) -> IndexEntry {
    let line_bytes = line.len();
    IndexEntry {
        file,
        stamp,
        summary,
        line,
        line_bytes,
    }
}

/// Serialize one entry (always representable here: Rust summaries hold only finite numbers).
pub fn to_index_entry(
    file: &str,
    stamp: FileStamp,
    summary: &SessionSummary,
) -> Option<IndexEntry> {
    let line = format!(
        "{}\n",
        json!({ "file": file, "size": stamp.size, "mtimeMs": stamp.mtime_ms, "summary": summary_to_json(summary) })
    );
    Some(with_line(file.to_owned(), stamp, summary.clone(), line))
}

fn parse_entry_line(line: &str) -> Option<IndexEntry> {
    let parsed: Value = serde_json::from_str(line).ok()?;
    let file = parsed.get("file")?.as_str()?;
    if file.contains('/') {
        return None;
    }
    let size = parsed
        .get("size")?
        .as_f64()
        .filter(|size| size.is_finite() && *size >= 0.0)?;
    let mtime_ms = parsed
        .get("mtimeMs")?
        .as_f64()
        .filter(|mtime| mtime.is_finite())?;
    let summary = summary_from_json(parsed.get("summary")?)?;
    Some(with_line(
        file.to_owned(),
        FileStamp {
            size: size as u64,
            mtime_ms,
        },
        summary,
        format!("{line}\n"),
    ))
}

fn is_current_header(line: &str) -> bool {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|parsed| parsed.get("version").and_then(Value::as_u64))
        == Some(INDEX_VERSION)
}

/// Load an index, tolerating every failure: a missing file, a missing or foreign version header, or a
/// read error loads as unusable; a line that does not parse (a torn append, garbage) is skipped.
pub fn load_index_file(path: &str) -> LoadedIndex {
    let Ok(bytes) = std::fs::read(path) else {
        return LoadedIndex::unusable();
    };
    let content = String::from_utf8_lossy(&bytes);
    let mut lines = content.split('\n');
    let Some(header) = lines.next().filter(|_| !bytes.is_empty()) else {
        return LoadedIndex::unusable();
    };
    if !is_current_header(header) {
        return LoadedIndex::unusable();
    }
    let header_bytes = header.len() + 1;
    let mut entries = HashMap::new();
    for line in lines {
        if let Some(entry) = parse_entry_line(line) {
            entries.insert(entry.file.clone(), entry);
        }
    }
    LoadedIndex {
        usable: true,
        entries,
        entry_bytes: bytes.len().saturating_sub(header_bytes),
        ends_with_newline: bytes.last() == Some(&b'\n'),
    }
}

/// Append entries one whole line per write, first closing a torn tail when there is one.
pub fn append_index_entries(
    path: &str,
    entries: &[&IndexEntry],
    ends_with_newline: bool,
) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?;
    let mut prefix = if ends_with_newline { "" } else { "\n" };
    for entry in entries {
        file.write_all(format!("{prefix}{}", entry.line).as_bytes())?;
        prefix = "";
    }
    Ok(())
}

pub fn rewrite_index_file(path: &str, entries: &[&IndexEntry]) -> io::Result<()> {
    let temp = format!(
        "{path}.tmp-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    let content: String = std::iter::once(header_line())
        .chain(entries.iter().map(|entry| entry.line.clone()))
        .collect();
    let written = std::fs::write(&temp, content).and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn summary(id: &str, last_activity_time: Option<i64>) -> SessionSummary {
        SessionSummary {
            header: json!({"type": "session", "id": id, "timestamp": "2026-10-30T00:00:00.000Z"}),
            name: Some("named".to_owned()),
            first_user_message: "hi".to_owned(),
            message_count: 2,
            last_activity_time,
            all_messages_text: "hi there".to_owned(),
            repository_identity: None,
        }
    }

    #[test]
    fn round_trips_entries_and_skips_torn_and_foreign_lines() {
        let Some(dir) = tempfile::tempdir().ok() else {
            panic!("tempdir")
        };
        let path = dir
            .path()
            .join(SESSION_SUMMARY_INDEX_FILE)
            .to_string_lossy()
            .into_owned();
        assert!(!load_index_file(&path).usable);
        let stamp = FileStamp {
            size: 10,
            mtime_ms: 5.5,
        };
        let Some(a) = to_index_entry("a.jsonl", stamp, &summary("a", Some(7))) else {
            panic!("entry")
        };
        let Some(b) = to_index_entry("b.jsonl", stamp, &summary("b", None)) else {
            panic!("entry")
        };
        assert!(rewrite_index_file(&path, &[&a]).is_ok());
        assert!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(b"{\"torn"))
                .is_ok()
        );
        let loaded = load_index_file(&path);
        assert!(loaded.usable && !loaded.ends_with_newline);
        assert_eq!(loaded.entries.get("a.jsonl"), Some(&a));
        assert!(append_index_entries(&path, &[&b], loaded.ends_with_newline).is_ok());
        let loaded = load_index_file(&path);
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(
            loaded.entries.get("b.jsonl").map(|e| &e.summary),
            Some(&b.summary)
        );
        assert!(std::fs::write(&path, "{\"version\":1}\n").is_ok());
        assert!(!load_index_file(&path).usable);
    }
}
