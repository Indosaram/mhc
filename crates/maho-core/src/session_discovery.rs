//! Port of senpi packages/coding-agent/src/core/session-discovery.ts.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::session_summary_cache::read_cached_session_summary;

/// senpi `SessionListProgress`: `(loaded, total)` as each session file is read.
pub type SessionListProgress<'a> = &'a mut dyn FnMut(usize, usize);

pub const MAX_CONCURRENT_SESSION_INFO_LOADS: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub struct SessionInfo {
    pub path: String,
    pub id: String,
    pub cwd: String,
    pub name: Option<String>,
    pub parent_session_path: Option<String>,
    pub created: DateTime<Utc>,
    pub modified: DateTime<Utc>,
    pub message_count: usize,
    pub first_message: String,
    pub all_messages_text: String,
}

fn resolve_modified(activity_time: Option<i64>, header: &Value, mtime: DateTime<Utc>) -> DateTime<Utc> {
    if let Some(activity) = activity_time.filter(|activity| *activity > 0) {
        return DateTime::<Utc>::from_timestamp_millis(activity).unwrap_or(mtime);
    }
    let header_time = header
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|stamp| DateTime::parse_from_rfc3339(stamp).ok())
        .map(|parsed| parsed.with_timezone(&Utc));
    header_time.unwrap_or(mtime)
}

/// Builds one picker row from a session file's exact summary, or None when the file is not a
/// session.
pub fn build_session_info(file_path: &str) -> Option<SessionInfo> {
    build_session_info_with_text(file_path, true)
}

fn build_session_info_with_text(file_path: &str, include_text: bool) -> Option<SessionInfo> {
    let cached = read_cached_session_summary(file_path)?;
    let header = &cached.summary.header;
    let created = header
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|stamp| DateTime::parse_from_rfc3339(stamp).ok())
        .map(|parsed| parsed.with_timezone(&Utc))
        .unwrap_or(cached.mtime);
    Some(SessionInfo {
        path: file_path.to_owned(),
        id: header.get("id").and_then(Value::as_str).unwrap_or_default().to_owned(),
        cwd: header.get("cwd").and_then(Value::as_str).unwrap_or_default().to_owned(),
        name: cached.summary.name.clone(),
        parent_session_path: header.get("parentSession").and_then(Value::as_str).map(str::to_owned),
        created,
        modified: resolve_modified(cached.summary.last_activity_time, header, cached.mtime),
        message_count: cached.summary.message_count,
        first_message: if cached.summary.first_user_message.is_empty() {
            "(no messages)".to_owned()
        } else {
            cached.summary.first_user_message.clone()
        },
        all_messages_text: if include_text { cached.summary.all_messages_text.clone() } else { String::new() },
    })
}

/// Builds picker rows for an explicit file list, dropping files that are not sessions.
pub fn list_session_infos(files: &[String], on_loaded: impl FnMut()) -> Vec<SessionInfo> {
    list_session_infos_with_text(files, true, on_loaded)
}

fn list_session_infos_with_text(files: &[String], include_text: bool, mut on_loaded: impl FnMut()) -> Vec<SessionInfo> {
    let mut sessions = Vec::new();
    for file in files {
        let info = build_session_info_with_text(file, include_text);
        on_loaded();
        if let Some(info) = info {
            sessions.push(info);
        }
    }
    sessions
}

/// Builds picker rows for every .jsonl file in one session directory.
pub fn list_sessions_from_dir(
    dir: &str,
    mut on_progress: Option<SessionListProgress<'_>>,
    progress_offset: usize,
    progress_total: Option<usize>,
) -> Vec<SessionInfo> {
    let files = sorted_session_files(dir);
    let total = progress_total.unwrap_or(files.len());
    let mut loaded = 0usize;
    list_session_infos(&files, || {
        loaded += 1;
        if let Some(progress) = on_progress.as_deref_mut() {
            progress(progress_offset + loaded, total);
        }
    })
}

/// Same rows as [`list_sessions_from_dir`] with `all_messages_text` left empty, for callers that
/// never search transcript text and should not copy it out of the summary cache.
pub fn list_sessions_from_dir_without_text(dir: &str) -> Vec<SessionInfo> {
    list_session_infos_with_text(&sorted_session_files(dir), false, || {})
}

fn sorted_session_files(dir: &str) -> Vec<String> {
    if !Path::new(dir).exists() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .map(|path: PathBuf| path.to_string_lossy().into_owned())
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(dir: &std::path::Path, name: &str, content: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, content).expect("write");
        path.to_string_lossy().into_owned()
    }

    fn session_content() -> String {
        [
            json!({ "type": "session", "id": "s1", "timestamp": "2020-01-01T00:00:00.000Z", "cwd": "/w" }).to_string(),
            json!({ "type": "message", "message": { "role": "user", "content": "hi", "timestamp": 5 } }).to_string(),
        ]
        .join("\n")
    }

    #[test]
    fn builds_a_row_from_a_session_file() {
        crate::session_summary_cache::clear_session_summary_cache();
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = write(tmp.path(), "a.jsonl", &session_content());
        let info = build_session_info(&path).expect("info");
        assert_eq!(info.id, "s1");
        assert_eq!(info.cwd, "/w");
        assert_eq!(info.message_count, 1);
        assert_eq!(info.first_message, "hi");
    }

    #[test]
    fn a_non_session_file_yields_no_row() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = write(tmp.path(), "b.jsonl", "not json");
        assert!(build_session_info(&path).is_none());
    }

    #[test]
    fn listing_a_directory_filters_to_jsonl_sessions() {
        crate::session_summary_cache::clear_session_summary_cache();
        let tmp = tempfile::tempdir().expect("tempdir");
        write(tmp.path(), "a.jsonl", &session_content());
        write(tmp.path(), "b.txt", "ignored");
        let sessions = list_sessions_from_dir(&tmp.path().to_string_lossy(), None, 0, None);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "s1");
    }

    #[test]
    fn the_text_free_listing_matches_except_for_the_transcript_text() {
        crate::session_summary_cache::clear_session_summary_cache();
        let tmp = tempfile::tempdir().expect("tempdir");
        write(tmp.path(), "a.jsonl", &session_content());
        let dir = tmp.path().to_string_lossy().into_owned();
        let full = list_sessions_from_dir(&dir, None, 0, None);
        let lean = list_sessions_from_dir_without_text(&dir);
        assert_eq!(full[0].all_messages_text, "hi");
        assert_eq!(lean[0].all_messages_text, "");
        assert_eq!(SessionInfo { all_messages_text: String::new(), ..full[0].clone() }, lean[0]);
    }

    #[test]
    fn listing_a_missing_directory_is_empty() {
        assert!(list_sessions_from_dir("/definitely/not/here", None, 0, None).is_empty());
    }
}
