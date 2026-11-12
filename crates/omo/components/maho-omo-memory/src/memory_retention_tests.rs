use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use memory_core::facts::{
    failures_schema::{
        FACTS_FAILURES_VERSION, FactsFailureRecord, FactsFailureState, render_failures_file,
    },
    schema::queue_entry_suffix,
};

use crate::memory_retention::{RetentionOptions, RetentionPaths, prune_runtime};

const NOW_MS: u64 = 1_800_000_000_000;

fn record(conversation: &str, end_message: &str, parked_at: &str) -> FactsFailureRecord {
    FactsFailureRecord {
        conversation_id: conversation.to_owned(),
        end_message_id: end_message.to_owned(),
        end_snapshot_line: 1,
        state: FactsFailureState::Parked,
        streak: 5,
        first_failure_at: parked_at.to_owned(),
        last_failure_at: parked_at.to_owned(),
        last_reason: memory_core::facts::failures_schema::FactsFailureReason::ChildExit,
        last_detail: None,
        last_failure_id: "id".to_owned(),
        next_eligible_at: None,
        parked_at: Some(parked_at.to_owned()),
    }
}

fn fixture(records: &[FactsFailureRecord]) -> (tempfile::TempDir, RetentionPaths) {
    let dir = tempfile::tempdir().unwrap();
    let queue = dir.path().join("facts-queue");
    let parked = queue.join("parked");
    fs::create_dir_all(&parked).unwrap();
    let locks = dir.path().join("locks");
    fs::create_dir_all(&locks).unwrap();
    let reflection = dir.path().join("reflection");
    fs::create_dir_all(&reflection).unwrap();
    let transcripts = dir.path().join("transcripts");
    fs::create_dir_all(&transcripts).unwrap();
    let rendered = render_failures_file(records, "2026-01-01T00:00:00.000Z");
    fs::write(queue.join("failures.json"), rendered).unwrap();
    for entry in records {
        let suffix = queue_entry_suffix(&entry.conversation_id, &entry.end_message_id);
        fs::write(
            parked.join(format!("20260101T000000000Z{suffix}")),
            b"payload",
        )
        .unwrap();
    }
    let paths = RetentionPaths {
        facts_queue: queue,
        locks,
        reflection,
        transcripts,
    };
    (dir, paths)
}

fn options() -> RetentionOptions {
    RetentionOptions {
        now_ms: NOW_MS,
        is_live_session: Arc::new(|_| false),
        ..Default::default()
    }
}

fn read_entries(paths: &RetentionPaths) -> Vec<FactsFailureRecord> {
    let raw = fs::read_to_string(paths.facts_queue.join("failures.json")).unwrap();
    memory_core::facts::failures_schema::parse_failures_file(&raw)
        .unwrap()
        .entries
}

#[test]
fn parked_payloads_past_the_horizon_expire_with_their_ledger_entries() {
    let old = record("conv-old", "msg-old", "2026-01-01T00:00:00.000Z");
    let recent = record("conv-new", "msg-new", "2027-06-01T00:00:00.000Z");
    let (_dir, paths) = fixture(&[old, recent]);

    let report = prune_runtime(&paths, &options());

    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.parked_entries_removed, 1);
    let entries = read_entries(&paths);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].conversation_id, "conv-new");
    let parked: Vec<_> = fs::read_dir(paths.facts_queue.join("parked"))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(parked.len(), 1);
}

#[test]
fn parked_payloads_left_in_the_queue_root_by_an_older_ledger_are_removed_too() {
    let old = record("conv-old", "msg-old", "2026-01-01T00:00:00.000Z");
    let (_dir, paths) = fixture(std::slice::from_ref(&old));
    let parked = paths.facts_queue.join("parked");
    for file in fs::read_dir(&parked).unwrap().filter_map(Result::ok) {
        fs::rename(file.path(), paths.facts_queue.join(file.file_name())).unwrap();
    }
    let leftover = paths.facts_queue.join(format!(
        "20260101T000000000Z{}",
        queue_entry_suffix(&old.conversation_id, &old.end_message_id)
    ));
    old_mtime(&leftover);

    let report = prune_runtime(&paths, &options());

    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.parked_entries_removed, 1);
    assert!(!leftover.exists());
}

#[test]
fn a_requeued_payload_newer_than_the_cutoff_survives_the_ledger_expiry() {
    let old = record("conv-old", "msg-old", "2026-01-01T00:00:00.000Z");
    let (_dir, paths) = fixture(std::slice::from_ref(&old));
    let name = format!(
        "20260101T000000000Z{}",
        queue_entry_suffix(&old.conversation_id, &old.end_message_id)
    );
    let _ = fs::remove_file(paths.facts_queue.join("parked").join(&name));
    let requeued = paths.facts_queue.join(format!(
        "20270601T000000000Z{}",
        queue_entry_suffix(&old.conversation_id, &old.end_message_id)
    ));
    fs::write(&requeued, b"payload").unwrap();

    let report = prune_runtime(&paths, &options());

    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.parked_entries_removed, 1);
    assert!(requeued.exists());
}

#[test]
fn a_ledger_without_expired_entries_is_left_byte_identical() {
    let recent = record("conv-new", "msg-new", "2027-06-01T00:00:00.000Z");
    let (_dir, paths) = fixture(&[recent]);
    let before = fs::read(paths.facts_queue.join("failures.json")).unwrap();

    let report = prune_runtime(&paths, &options());

    assert!(report.is_empty());
    assert_eq!(
        fs::read(paths.facts_queue.join("failures.json")).unwrap(),
        before
    );
    assert_eq!(read_entries(&paths).len(), 1);
}

#[test]
fn a_corrupt_ledger_is_never_rewritten() {
    let (dir, paths) = fixture(&[]);
    fs::write(paths.facts_queue.join("failures.json"), b"{not json").unwrap();

    prune_runtime(&paths, &options());

    assert_eq!(
        fs::read(paths.facts_queue.join("failures.json")).unwrap(),
        b"{not json"
    );
    drop(dir);
}

#[test]
fn absent_runtime_trees_are_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RetentionPaths {
        facts_queue: dir.path().join("missing-queue"),
        locks: dir.path().join("missing-locks"),
        reflection: dir.path().join("missing-reflection"),
        transcripts: dir.path().join("missing-transcripts"),
    };

    assert!(prune_runtime(&paths, &options()).is_empty());
}

/// Applies to files and directories alike: the retention scan mtime-checks both, and Windows only
/// allows a directory handle to be opened for `FILE_WRITE_ATTRIBUTES` (never `GENERIC_WRITE`, which
/// is what `filetime::set_file_mtime` asks for), so directories take a dedicated path there.
fn set_mtime(path: &PathBuf, at_ms: u64) {
    let at = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(at_ms);
    let at = filetime::FileTime::from_system_time(at);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // `FILE_WRITE_ATTRIBUTES` plus `FILE_FLAG_BACKUP_SEMANTICS`: the latter is what makes a
        // directory openable at all, the former is all `SetFileTime` needs.
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0000_0100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap();
        filetime::set_file_handle_times(&file, None, Some(at)).unwrap();
    }
    #[cfg(not(windows))]
    {
        filetime::set_file_mtime(path, at).unwrap();
    }
}

/// File times are derived from the injected clock so no test depends on wall time.
fn old_mtime(path: &PathBuf) {
    set_mtime(path, NOW_MS - 40 * 24 * 60 * 60 * 1000);
}

fn fresh_mtime(path: &PathBuf) {
    set_mtime(path, NOW_MS - 24 * 60 * 60 * 1000);
}

#[test]
fn finished_reflection_runs_past_the_horizon_are_removed_but_unfinished_and_live_runs_survive() {
    let (_dir, paths) = fixture(&[]);
    let runs = paths.reflection.join("runs");
    let finished = runs.join("reflection-run-1");
    let unfinished = runs.join("reflection-run-2");
    let live = runs.join("reflection-run-3");
    for run in [&finished, &unfinished, &live] {
        fs::create_dir_all(run).unwrap();
        fs::write(run.join("ledger.json"), b"{}").unwrap();
        old_mtime(&run.join("ledger.json"));
        old_mtime(run);
    }
    fs::write(finished.join("final.json"), b"{}").unwrap();
    fs::write(live.join("final.json"), b"{}").unwrap();
    fs::write(
        paths.reflection.join("active.lock"),
        br#"{"runId":"reflection-run-3"}"#,
    )
    .unwrap();

    let report = prune_runtime(&paths, &options());

    assert_eq!(report.reflection_runs_removed, 1);
    assert!(!finished.exists());
    assert!(unfinished.exists());
    assert!(live.exists());
}

#[test]
fn transcripts_past_the_horizon_are_removed_and_live_sessions_are_kept() {
    let (_dir, paths) = fixture(&[]);
    let stale = paths.transcripts.join("session-old");
    let live = paths.transcripts.join("session-live");
    for session in [&stale, &live] {
        fs::create_dir_all(session).unwrap();
        let file = session.join("journal.jsonl");
        fs::write(&file, b"{}\n").unwrap();
        old_mtime(&file);
    }
    let mut options = options();
    options.is_live_session = Arc::new(|session| session == "session-live");

    let report = prune_runtime(&paths, &options);

    assert_eq!(report.transcripts_removed, 1);
    assert!(!stale.exists());
    assert!(live.exists());
}

#[test]
fn a_session_is_kept_whole_when_any_file_is_recent() {
    let (_dir, paths) = fixture(&[]);
    let session = paths.transcripts.join("session-mixed");
    fs::create_dir_all(&session).unwrap();
    let journal = session.join("transcript.jsonl");
    fs::write(&journal, b"{}\n").unwrap();
    old_mtime(&journal);
    fs::write(session.join("state.json"), b"{}").unwrap();
    fresh_mtime(&session.join("state.json"));

    let report = prune_runtime(&paths, &options());

    assert_eq!(report.transcripts_removed, 0);
    assert!(journal.exists());
    assert!(session.join("state.json").exists());
}

#[test]
fn a_locked_session_is_never_removed() {
    let (_dir, paths) = fixture(&[]);
    let session = paths.transcripts.join("session-locked");
    fs::create_dir_all(&session).unwrap();
    for name in ["transcript.jsonl", "state.json", "state.lock"] {
        let file = session.join(name);
        fs::write(&file, b"{}").unwrap();
        old_mtime(&file);
    }

    let report = prune_runtime(&paths, &options());

    assert_eq!(report.transcripts_removed, 0);
    assert!(session.exists());
}

#[test]
fn empty_journal_ledger_metadata_is_preserved() {
    let (_dir, paths) = fixture(&[]);
    let rendered = format!(
        "{{\"version\":{FACTS_FAILURES_VERSION},\"updatedAt\":\"2026-02-02T00:00:00.000Z\",\"entries\":[]}}"
    );
    fs::write(paths.facts_queue.join("failures.json"), rendered).unwrap();

    prune_runtime(&paths, &options());

    let raw = fs::read_to_string(paths.facts_queue.join("failures.json")).unwrap();
    assert!(raw.contains("2026-02-02T00:00:00.000Z"));
}

#[test]
fn cutoff_uses_the_configured_day_count() {
    let old = record("conv-old", "msg-old", "2026-01-01T00:00:00.000Z");
    let (_dir, paths) = fixture(&[old]);
    let mut options = options();
    options.parked_days = 100_000;

    let report = prune_runtime(&paths, &options);

    assert_eq!(report.parked_entries_removed, 0);
    assert_eq!(read_entries(&paths).len(), 1);
}
