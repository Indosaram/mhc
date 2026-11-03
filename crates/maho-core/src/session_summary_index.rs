//! Port of senpi packages/coding-agent/src/core/session-summary-index.ts.
//!
//! Persistent summaries for one sessions directory, stored as `<dir>/.session-summaries.index`
//! (senpi#2087): the second cache level behind the in-memory LRU, so a cold process lists an unchanged
//! directory by reading one file. Nothing here fails or logs: an unreadable, corrupt or unwritable
//! index only means the listing streams, and its rows are identical either way.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use crate::session_summary::SessionSummary;
pub use crate::session_summary_index_file::SESSION_SUMMARY_INDEX_FILE;
use crate::session_summary_index_file::{
    IndexEntry, LoadedIndex, append_index_entries, load_index_file, rewrite_index_file,
    to_index_entry,
};
use crate::session_summary_lru::FileStamp;

/// Upper bound on live index bytes per sessions directory; past it the least recently active sessions
/// are left out and stream like any cache miss.
pub const SESSION_SUMMARY_INDEX_MAX_BYTES: usize = 256 * 1024 * 1024;

/// What this process last saw in one index file: every entry's stamp, trusted only while the index file
/// keeps the size and mtime recorded with it.
#[derive(Clone)]
struct IndexSnapshot {
    index_stamp: FileStamp,
    entry_stamps: HashMap<String, FileStamp>,
}

static SNAPSHOTS: LazyLock<Mutex<HashMap<String, IndexSnapshot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn snapshots() -> std::sync::MutexGuard<'static, HashMap<String, IndexSnapshot>> {
    SNAPSHOTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Forget every index snapshot, as a fresh process would start.
pub fn forget_session_summary_index_snapshots() {
    snapshots().clear();
}

fn stamps_match(left: &FileStamp, right: &FileStamp) -> bool {
    left.size == right.size && left.mtime_ms == right.mtime_ms
}

fn stat_stamp(path: &str) -> Option<FileStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    let mtime_ms = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64() * 1000.0)
        .unwrap_or(0.0);
    Some(FileStamp {
        size: metadata.len(),
        mtime_ms,
    })
}

fn recency(entry: &IndexEntry) -> i64 {
    entry.summary.last_activity_time.unwrap_or_else(|| {
        entry
            .summary
            .header
            .get("timestamp")
            .and_then(serde_json::Value::as_str)
            .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
            .map_or(0, |time| time.timestamp_millis())
    })
}

fn select_within_budget<'a>(entries: &[&'a IndexEntry], max_bytes: usize) -> Vec<&'a IndexEntry> {
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|entry| std::cmp::Reverse(recency(entry)));
    let mut kept = Vec::new();
    let mut bytes = 0;
    for entry in sorted {
        if entry.line_bytes > max_bytes {
            continue;
        }
        if bytes + entry.line_bytes > max_bytes {
            break;
        }
        bytes += entry.line_bytes;
        kept.push(entry);
    }
    kept
}

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub struct SessionSummaryIndex {
    path: String,
    max_bytes: usize,
    loaded: Option<LoadedIndex>,
    snapshot: Option<Option<IndexSnapshot>>,
    recorded: Vec<(String, FileStamp, SessionSummary)>,
}

impl SessionSummaryIndex {
    pub fn new(dir: &str, max_bytes: Option<usize>) -> Self {
        Self {
            path: Path::new(dir)
                .join(SESSION_SUMMARY_INDEX_FILE)
                .to_string_lossy()
                .into_owned(),
            max_bytes: max_bytes.unwrap_or(SESSION_SUMMARY_INDEX_MAX_BYTES),
            loaded: None,
            snapshot: None,
            recorded: Vec::new(),
        }
    }

    /// The indexed summary of `file_path` when it was indexed at exactly `stamp`. A warm process whose
    /// snapshot shows the index cannot serve the file skips loading the index.
    pub fn lookup(&mut self, file_path: &str, stamp: FileStamp) -> Option<SessionSummary> {
        let file = basename(file_path);
        if self.loaded.is_none() {
            if self.snapshot.is_none() {
                self.snapshot = Some(self.current_snapshot());
            }
            if let Some(Some(known)) = &self.snapshot
                && !known
                    .entry_stamps
                    .get(&file)
                    .is_some_and(|indexed| stamps_match(indexed, &stamp))
            {
                return None;
            }
        }
        let loaded = self.loaded();
        let entry = loaded.entries.get(&file)?;
        stamps_match(&entry.stamp, &stamp).then(|| entry.summary.clone())
    }

    pub fn record(&mut self, file_path: &str, stamp: FileStamp, summary: SessionSummary) {
        self.recorded.push((basename(file_path), stamp, summary));
    }

    /// Bring the index up to date after a listing of `present_files`. Summaries the index lacks are
    /// appended one line each; the file is rewritten instead when it is unusable, or when its entry bytes
    /// exceed twice the live entries' bytes (superseded, torn, removed and over-budget lines are dead).
    pub fn persist(&mut self, present_files: &[String]) {
        let Some(loaded) = self.loaded.take() else {
            return;
        };
        let mut entries = loaded.entries.clone();
        let mut added: HashSet<String> = HashSet::new();
        for (file, stamp, summary) in &self.recorded {
            if entries
                .get(file)
                .is_some_and(|existing| stamps_match(&existing.stamp, stamp))
            {
                continue;
            }
            let Some(entry) = to_index_entry(file, *stamp, summary) else {
                continue;
            };
            entries.insert(file.clone(), entry);
            added.insert(file.clone());
        }
        let present: HashSet<String> = present_files.iter().map(|path| basename(path)).collect();
        let live: Vec<&IndexEntry> = entries
            .values()
            .filter(|entry| present.contains(&entry.file))
            .collect();
        let kept = select_within_budget(&live, self.max_bytes);
        let kept_bytes: usize = kept.iter().map(|entry| entry.line_bytes).sum();
        let appended: Vec<&IndexEntry> = kept
            .iter()
            .copied()
            .filter(|entry| added.contains(&entry.file))
            .collect();
        let appended_bytes: usize = appended.iter().map(|entry| entry.line_bytes).sum();

        let result = if (!loaded.usable && !kept.is_empty())
            || (loaded.usable && loaded.entry_bytes + appended_bytes > 2 * kept_bytes)
        {
            rewrite_index_file(&self.path, &kept).map(|()| {
                self.remember(
                    stat_stamp(&self.path),
                    Some(kept.iter().map(|entry| (entry.file.clone(), entry.stamp))),
                )
            })
        } else if loaded.usable && !appended.is_empty() {
            append_index_entries(&self.path, &appended, loaded.ends_with_newline).map(|()| {
                let mut in_file: HashMap<String, FileStamp> = loaded
                    .entries
                    .values()
                    .map(|entry| (entry.file.clone(), entry.stamp))
                    .collect();
                in_file.extend(
                    appended
                        .iter()
                        .map(|entry| (entry.file.clone(), entry.stamp)),
                );
                self.remember(stat_stamp(&self.path), Some(in_file.into_iter()))
            })
        } else {
            Ok(())
        };
        if result.is_err() {
            snapshots().remove(&self.path);
        }
        self.loaded = Some(loaded);
    }

    fn current_snapshot(&self) -> Option<IndexSnapshot> {
        let recorded = snapshots().get(&self.path).cloned()?;
        let current = stat_stamp(&self.path)?;
        stamps_match(&current, &recorded.index_stamp).then_some(recorded)
    }

    /// Stat before reading, so a concurrent writer can only make the snapshot look stale.
    fn loaded(&mut self) -> &LoadedIndex {
        if self.loaded.is_none() {
            let before = stat_stamp(&self.path);
            let loaded = load_index_file(&self.path);
            let stamps: Vec<(String, FileStamp)> = loaded
                .entries
                .values()
                .map(|entry| (entry.file.clone(), entry.stamp))
                .collect();
            self.remember(before, loaded.usable.then_some(stamps.into_iter()));
            self.loaded = Some(loaded);
        }
        self.loaded.get_or_insert_with(|| LoadedIndex {
            usable: false,
            entries: HashMap::new(),
            entry_bytes: 0,
            ends_with_newline: true,
        })
    }

    fn remember(
        &self,
        index_stamp: Option<FileStamp>,
        entries: Option<impl Iterator<Item = (String, FileStamp)>>,
    ) {
        let (Some(index_stamp), Some(entries)) = (index_stamp, entries) else {
            snapshots().remove(&self.path);
            return;
        };
        snapshots().insert(
            self.path.clone(),
            IndexSnapshot {
                index_stamp,
                entry_stamps: entries.collect(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_summary_index_file::tests::summary;

    #[test]
    fn a_cold_index_serves_recorded_summaries_and_drops_removed_sessions() {
        let Some(dir) = tempfile::tempdir().ok() else {
            panic!("tempdir")
        };
        let dir = dir.path().to_string_lossy().into_owned();
        let stamp = FileStamp {
            size: 3,
            mtime_ms: 9.0,
        };
        let mut index = SessionSummaryIndex::new(&dir, None);
        assert_eq!(index.lookup(&format!("{dir}/a.jsonl"), stamp), None);
        index.record(&format!("{dir}/a.jsonl"), stamp, summary("a", Some(2)));
        index.record(&format!("{dir}/b.jsonl"), stamp, summary("b", Some(1)));
        index.persist(&[format!("{dir}/a.jsonl"), format!("{dir}/b.jsonl")]);

        forget_session_summary_index_snapshots();
        let mut cold = SessionSummaryIndex::new(&dir, None);
        assert_eq!(
            cold.lookup(&format!("{dir}/a.jsonl"), stamp),
            Some(summary("a", Some(2)))
        );
        assert_eq!(
            cold.lookup(
                &format!("{dir}/a.jsonl"),
                FileStamp {
                    size: 4,
                    mtime_ms: 9.0
                }
            ),
            None
        );
        cold.persist(&[format!("{dir}/a.jsonl")]);
        // One dead line of the same size is below the 2x compaction threshold: the file is left alone.
        let loaded = load_index_file(&format!("{dir}/{SESSION_SUMMARY_INDEX_FILE}"));
        assert!(loaded.entries.contains_key("a.jsonl"));
        assert!(loaded.entries.contains_key("b.jsonl"));
    }

    #[test]
    fn the_byte_budget_keeps_the_most_recently_active_sessions() {
        let Some(dir) = tempfile::tempdir().ok() else {
            panic!("tempdir")
        };
        let dir = dir.path().to_string_lossy().into_owned();
        let stamp = FileStamp {
            size: 1,
            mtime_ms: 1.0,
        };
        let Some(one) = to_index_entry("new.jsonl", stamp, &summary("n", Some(5))) else {
            panic!("entry")
        };
        let mut index = SessionSummaryIndex::new(&dir, Some(one.line_bytes * 3 / 2));
        let _ = index.lookup(&format!("{dir}/old.jsonl"), stamp);
        index.record(&format!("{dir}/old.jsonl"), stamp, summary("o", Some(1)));
        index.record(&format!("{dir}/new.jsonl"), stamp, summary("n", Some(5)));
        index.persist(&[format!("{dir}/old.jsonl"), format!("{dir}/new.jsonl")]);
        let loaded = load_index_file(&format!("{dir}/{SESSION_SUMMARY_INDEX_FILE}"));
        let files: Vec<&String> = loaded.entries.keys().collect();
        assert_eq!(files, vec!["new.jsonl"]);
    }
}
