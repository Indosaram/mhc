//! Port of senpi packages/coding-agent/src/core/session-summary-cache.ts.

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Utc};

use crate::session_summary::{SessionSummary, read_session_summary};
use crate::session_summary_lru::{FileStamp, SessionSummaryLru, SummaryCacheBudget, SummaryEntry};

pub const SESSION_SUMMARY_CACHE_LIMIT: usize = 4096;
pub const SESSION_SUMMARY_CACHE_MAX_TEXT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct CachedSessionSummary {
    pub summary: SessionSummary,
    pub mtime: DateTime<Utc>,
}

fn cache() -> &'static Mutex<SessionSummaryLru> {
    static CACHE: OnceLock<Mutex<SessionSummaryLru>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(SessionSummaryLru::new(SummaryCacheBudget {
            max_entries: SESSION_SUMMARY_CACHE_LIMIT,
            max_text_bytes: SESSION_SUMMARY_CACHE_MAX_TEXT_BYTES,
        }))
    })
}

fn lock() -> std::sync::MutexGuard<'static, SessionSummaryLru> {
    cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn stamps_match(left: FileStamp, right: FileStamp) -> bool {
    left.size == right.size && left.mtime_ms == right.mtime_ms
}

fn mtime_of(metadata: &std::fs::Metadata) -> DateTime<Utc> {
    metadata.modified().map(DateTime::<Utc>::from).unwrap_or_else(|_| Utc::now())
}

/// Exact summary of one session file, reusing the cached summary when the file's size and mtime are
/// unchanged since it was read. A file that cannot be stat'ed or read drops its cache entry.
pub fn read_cached_session_summary(file_path: &str) -> Option<CachedSessionSummary> {
    let key = std::fs::canonicalize(file_path)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| Path::new(file_path).to_string_lossy().into_owned());

    let metadata = match std::fs::metadata(file_path) {
        Ok(metadata) => metadata,
        Err(_) => {
            lock().drop_key(&key);
            return None;
        }
    };
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs_f64() * 1000.0)
        .unwrap_or(0.0);
    let stamp = FileStamp { size: metadata.len(), mtime_ms };
    let mtime = mtime_of(&metadata);

    if let Some(cached) = lock().get(&key)
        && stamps_match(cached.stamp, stamp)
    {
        return Some(CachedSessionSummary { summary: cached.summary, mtime: cached.mtime });
    }

    let summary = read_session_summary(file_path)?;
    lock().retain(&key, SummaryEntry { stamp, summary: summary.clone(), mtime });
    Some(CachedSessionSummary { summary, mtime })
}

pub fn clear_session_summary_cache() {
    lock().clear();
}

pub fn session_summary_cache_size() -> usize {
    lock().size()
}

pub fn session_summary_cache_text_bytes() -> usize {
    lock().text_bytes()
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

    #[test]
    fn an_unchanged_file_is_served_from_the_cache() {
        clear_session_summary_cache();
        let tmp = tempfile::tempdir().expect("tempdir");
        let content = [
            json!({ "type": "session", "id": "s1", "timestamp": "2020-01-01T00:00:00.000Z" }).to_string(),
            json!({ "type": "message", "message": { "role": "user", "content": "hi", "timestamp": 1 } }).to_string(),
        ]
        .join("\n");
        let path = write(tmp.path(), "a.jsonl", &content);
        let first = read_cached_session_summary(&path).expect("first");
        assert_eq!(first.summary.message_count, 1);
        let second = read_cached_session_summary(&path).expect("second");
        assert_eq!(second.summary, first.summary);
    }

    #[test]
    fn a_missing_file_is_none() {
        assert!(read_cached_session_summary("/definitely/not/here.jsonl").is_none());
    }

    #[test]
    fn a_non_session_file_is_none() {
        clear_session_summary_cache();
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = write(tmp.path(), "b.jsonl", "not json");
        assert!(read_cached_session_summary(&path).is_none());
    }
}
