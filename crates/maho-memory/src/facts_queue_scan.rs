//! Finding the queue files an extraction run should consider.
//!
//! Spec: `docs/cleanroom/specs/memory-facts-queue-scan.md`. Filtering happens on the **file name** before any
//! payload is read: the queue directory can hold thousands of entries and a conversation-scoped run only
//! wants its own, so reading and parsing every file first would pay for the whole queue to use a handful.

use std::path::{Path, PathBuf};

use crate::facts_schema::{FactsQueueEntry, parse_queue_entry, queue_conversation_hash};

const NON_ENTRY_FILES: [&str; 3] = ["consumed.json", "failures.json", "claims.json"];

#[derive(Debug)]
pub struct ScannedQueueEntry {
    pub entry: FactsQueueEntry,
    pub file_path: PathBuf,
}

/// Every parseable entry in `dir`, sorted by file name.
///
/// Name order is timestamp order (the enqueue timestamp is part of the name), so callers get entries in the
/// order they arrived without sorting payloads.
///
/// A missing directory is empty, not an error: a conversation that has never produced facts has no queue
/// directory, and that is the normal first-run state.
pub fn scan_queue_entries(
    dir: &Path,
    conversation_id: Option<&str>,
) -> std::io::Result<Vec<ScannedQueueEntry>> {
    let directory = match std::fs::read_dir(dir) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut names = directory
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    names.sort();

    let wanted = conversation_id.map(|id| format!("-{}-", queue_conversation_hash(id)));
    let mut scanned = Vec::new();
    for name in names {
        let name = name.to_string_lossy();
        if !name.ends_with(".json") || NON_ENTRY_FILES.contains(&name.as_ref()) {
            continue;
        }
        if wanted
            .as_ref()
            .is_some_and(|wanted| !name.contains(wanted.as_str()))
        {
            continue;
        }
        let file_path = dir.join(name.as_ref());
        let Some(entry) = std::fs::read_to_string(&file_path)
            .ok()
            .and_then(|raw| parse_queue_entry(&raw))
        else {
            continue;
        };
        scanned.push(ScannedQueueEntry { entry, file_path });
    }
    Ok(scanned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts_schema::queue_timestamp;
    use std::fs;

    fn entry_json(conversation_id: &str) -> String {
        serde_json::json!({
            "version": 1,
            "identity": "host-1",
            "sessionId": "${conversation_id}-session",
            "conversationId": conversation_id,
            "range": {
                "start_message_id": "m1",
                "end_message_id": "m2",
                "start_line": 0,
                "end_snapshot_line": 1
            },
            "enqueuedAt": "2026-11-01T00:00:00Z"
        })
        .to_string()
    }

    /// Mirrors the real naming rule: the timestamp is part of the name so a re-enqueue is a new
    /// file, and `:` is stripped because it is illegal in a file name on Windows.
    fn entry_name(conversation_id: &str, at: &str) -> String {
        let hash = queue_conversation_hash(conversation_id);
        format!("{}-{hash}-m2.json", queue_timestamp(at))
    }

    #[test]
    fn a_missing_directory_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("nope");
        assert!(scan_queue_entries(&missing, None).expect("ok").is_empty());
    }

    #[test]
    fn an_empty_directory_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(scan_queue_entries(dir.path(), None).expect("ok").is_empty());
    }

    #[test]
    fn every_entry_is_returned_in_name_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        for at in ["2026-11-02T00:00:00Z", "2026-11-01T00:00:00Z"] {
            fs::write(dir.path().join(entry_name("c1", at)), entry_json("c1")).expect("write");
        }
        let scanned = scan_queue_entries(dir.path(), None).expect("ok");
        assert_eq!(scanned.len(), 2);
        assert!(
            scanned[0]
                .file_path
                .file_name()
                .expect("name")
                .to_string_lossy()
                .starts_with(&queue_timestamp("2026-11-01T00:00:00Z")),
            "name order is timestamp order"
        );
    }

    #[test]
    fn the_bookkeeping_files_are_not_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in NON_ENTRY_FILES {
            fs::write(dir.path().join(name), "{}").expect("write");
        }
        assert!(scan_queue_entries(dir.path(), None).expect("ok").is_empty());
    }

    #[test]
    fn a_non_json_file_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("notes.txt"), "x").expect("write");
        assert!(scan_queue_entries(dir.path(), None).expect("ok").is_empty());
    }

    #[test]
    fn a_conversation_filter_keeps_only_that_conversation() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path()
                .join(entry_name("wanted", "2026-11-01T00:00:00Z")),
            entry_json("wanted"),
        )
        .expect("write");
        fs::write(
            dir.path().join(entry_name("other", "2026-11-02T00:00:00Z")),
            entry_json("other"),
        )
        .expect("write");
        let scanned = scan_queue_entries(dir.path(), Some("wanted")).expect("ok");
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].entry.conversation_id, "wanted");
    }

    #[test]
    fn a_conversation_filter_matching_nothing_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join(entry_name("other", "2026-11-01T00:00:00Z")),
            entry_json("other"),
        )
        .expect("write");
        assert!(
            scan_queue_entries(dir.path(), Some("absent"))
                .expect("ok")
                .is_empty()
        );
    }

    #[test]
    fn a_malformed_entry_is_skipped_without_failing_the_scan() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join(entry_name("good", "2026-11-01T00:00:00Z")),
            entry_json("good"),
        )
        .expect("write");
        fs::write(dir.path().join("2026-11-02-badbeef-m9.json"), "{ not json").expect("write");
        let scanned = scan_queue_entries(dir.path(), None).expect("ok");
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].entry.conversation_id, "good");
    }

    #[test]
    fn an_entry_file_carries_its_path_for_later_removal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let name = entry_name("c1", "2026-11-01T00:00:00Z");
        fs::write(dir.path().join(&name), entry_json("c1")).expect("write");
        let scanned = scan_queue_entries(dir.path(), None).expect("ok");
        assert_eq!(scanned[0].file_path, dir.path().join(name));
    }
}
