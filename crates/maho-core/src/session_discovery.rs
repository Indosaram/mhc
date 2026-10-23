//! Port of senpi packages/coding-agent/src/core/session-discovery.ts.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::session_summary_cache::read_cached_session_summary;

/// senpi `SessionListProgress`: `(loaded, total, rows)` as each session file is read. senpi passes
/// the sorted cumulative partial list at each publication; here `rows` carries only the rows
/// finished since the previous publication, so each row is cloned once.
pub type SessionListProgress<'a> = &'a mut dyn FnMut(usize, usize, Option<Vec<SessionInfo>>);

pub const MAX_CONCURRENT_SESSION_INFO_LOADS: usize = 10;
/// Loaded-row counts between partial-list publications: one directory, and every directory.
pub const CURRENT_SESSION_LIST_PUBLISH_INTERVAL: usize = 10;
pub const ALL_SESSION_LIST_PUBLISH_INTERVAL: usize = 100;

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

/// Builds picker rows for an explicit file list, dropping files that are not sessions. `on_loaded`
/// runs once per file, in completion order.
pub fn list_session_infos(files: &[String], on_loaded: impl FnMut(Option<&SessionInfo>)) -> Vec<SessionInfo> {
    list_session_infos_with_text(files, true, on_loaded)
}

/// senpi `buildSessionInfosWithConcurrency`: up to [`MAX_CONCURRENT_SESSION_INFO_LOADS`] files load
/// at once, started in `files` order; the rows come back in `files` order.
fn list_session_infos_with_text(
    files: &[String],
    include_text: bool,
    mut on_loaded: impl FnMut(Option<&SessionInfo>),
) -> Vec<SessionInfo> {
    let mut results: Vec<Option<SessionInfo>> = (0..files.len()).map(|_| None).collect();
    let next = AtomicUsize::new(0);
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..MAX_CONCURRENT_SESSION_INFO_LOADS.min(files.len()) {
            let (sender, next) = (sender.clone(), &next);
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(file) = files.get(index) else { break };
                    if sender.send((index, build_session_info_with_text(file, include_text))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        for (index, info) in receiver {
            on_loaded(info.as_ref());
            results[index] = info;
        }
    });
    results.into_iter().flatten().collect()
}

/// Builds picker rows for `files` and reports `(progress_offset + loaded, total)` per file. Rows
/// are published on the first file, every `interval` files and the last file (senpi's partial-list
/// publication points).
pub fn list_session_files(
    files: &[String],
    mut on_progress: Option<SessionListProgress<'_>>,
    progress_offset: usize,
    total: usize,
    interval: usize,
) -> Vec<SessionInfo> {
    let mut loaded = 0usize;
    let mut pending = Vec::new();
    list_session_infos(files, |info| {
        loaded += 1;
        let Some(progress) = on_progress.as_deref_mut() else { return };
        pending.extend(info.cloned());
        let done = progress_offset + loaded;
        let publish = done == 1 || done.is_multiple_of(interval.max(1)) || done == total;
        progress(done, total, publish.then(|| std::mem::take(&mut pending)));
    })
}

/// Builds picker rows for every .jsonl file in one session directory.
pub fn list_sessions_from_dir(
    dir: &str,
    on_progress: Option<SessionListProgress<'_>>,
    progress_offset: usize,
    progress_total: Option<usize>,
) -> Vec<SessionInfo> {
    let files = session_files_newest_first(&[PathBuf::from(dir)]);
    let total = progress_total.unwrap_or(files.len());
    list_session_files(&files, on_progress, progress_offset, total, CURRENT_SESSION_LIST_PUBLISH_INTERVAL)
}

/// Same rows as [`list_sessions_from_dir`] with `all_messages_text` left empty, for callers that
/// never search transcript text and should not copy it out of the summary cache.
pub fn list_sessions_from_dir_without_text(dir: &str) -> Vec<SessionInfo> {
    list_session_infos_with_text(&session_files_newest_first(&[PathBuf::from(dir)]), false, |_| {})
}

/// Every .jsonl file directly inside `dirs`, newest modification time first.
///
/// Intentional mhc divergence: senpi starts row loads in readdir order, one directory after another.
/// mhc stats every file first (cheap next to parsing it) and starts loads newest first across all
/// `dirs`, so a progressive picker shows the sessions most likely to be resumed first. Listings
/// are still sorted by activity time afterwards, so only load and publication order differ.
pub fn session_files_newest_first(dirs: &[PathBuf]) -> Vec<String> {
    let mut files: Vec<(SystemTime, String)> = dirs
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|entries| entries.filter_map(Result::ok))
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .map(|path| {
            let modified = std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (modified, path.to_string_lossy().into_owned())
        })
        .collect();
    files.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    files.into_iter().map(|(_, path)| path).collect()
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
    fn session_files_are_ordered_newest_first_across_directories() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let nested = tmp.path().join("nested");
        std::fs::create_dir(&nested).expect("nested dir");
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for (dir, name, age) in [(tmp.path(), "old.jsonl", 30), (nested.as_path(), "newest.jsonl", 0), (tmp.path(), "mid.jsonl", 10)] {
            let path = write(dir, name, &session_content());
            let file = std::fs::File::options().write(true).open(&path).expect("open");
            file.set_modified(base - std::time::Duration::from_secs(age)).expect("set mtime");
        }
        write(tmp.path(), "ignored.txt", "x");
        let files = session_files_newest_first(&[tmp.path().to_path_buf(), nested]);
        let names: Vec<_> = files
            .iter()
            .filter_map(|file| std::path::Path::new(file).file_name()?.to_str())
            .collect();
        assert_eq!(names, ["newest.jsonl", "mid.jsonl", "old.jsonl"]);
    }

    #[test]
    fn progress_publishes_every_row_once_at_senpi_publication_points() {
        crate::session_summary_cache::clear_session_summary_cache();
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut files: Vec<String> = (0..25)
            .map(|index| write(tmp.path(), &format!("{index:02}.jsonl"), &session_content()))
            .collect();
        files.push(write(tmp.path(), "bad.jsonl", "not json"));
        let mut publications = Vec::new();
        let mut published_rows = 0;
        let sessions = list_session_files(
            &files,
            Some(&mut |loaded, total, rows: Option<Vec<SessionInfo>>| {
                assert_eq!(total, 26);
                if let Some(rows) = rows {
                    publications.push(loaded);
                    published_rows += rows.len();
                }
            }),
            0,
            files.len(),
            10,
        );
        assert_eq!(publications, [1, 10, 20, 26]);
        assert_eq!(published_rows, 25, "the non-session file yields no row");
        assert_eq!(sessions.len(), 25);
    }

    #[test]
    fn listing_a_missing_directory_is_empty() {
        assert!(list_sessions_from_dir("/definitely/not/here", None, 0, None).is_empty());
    }
}
