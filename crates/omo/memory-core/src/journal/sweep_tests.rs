use super::*;
use std::time::UNIX_EPOCH;

#[test]
fn writer_append_between_inspection_and_lock_is_rechecked() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("racing");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join(TRANSCRIPT), "").unwrap();
    std::fs::write(dir.join(STATE), "{}").unwrap();
    let row = "{\"kind\":\"user\",\"text\":\"hi\"}\n";
    let result = sweep_with_before_lock(root.path(), u64::MAX, DEFAULT_MIN_IDLE_MS, &|_| false, &|path| {
        std::fs::write(path.join(TRANSCRIPT), row).unwrap();
    }).unwrap();
    assert!(result.removed.is_empty());
    assert_eq!(result.kept.get(&EmptyJournalKeepReason::TranscriptContent), Some(&1));
    assert_eq!(std::fs::read_to_string(dir.join(TRANSCRIPT)).unwrap(), row);
}

#[test]
fn whitespace_transcript_and_zero_byte_state_are_removed() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("empty");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join(TRANSCRIPT), " \n\t").unwrap();
    std::fs::write(dir.join(STATE), "").unwrap();
    let result = sweep_empty_transcript_journals(root.path(), u64::MAX, DEFAULT_MIN_IDLE_MS, &|_| false).unwrap();
    assert_eq!(result.removed, ["empty"]);
    assert!(!dir.exists());
}

#[test]
fn state_only_empty_journal_is_removed() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("interrupted");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join(STATE), "{}").unwrap();
    let result = sweep_empty_transcript_journals(root.path(), u64::MAX, DEFAULT_MIN_IDLE_MS, &|_| false).unwrap();
    assert_eq!(result.removed, ["interrupted"]);
}

#[test]
fn recent_directory_or_file_is_kept() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("recent");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join(TRANSCRIPT), "").unwrap();
    let result = sweep_empty_transcript_journals(root.path(), 0, DEFAULT_MIN_IDLE_MS, &|_| false).unwrap();
    assert_eq!(result.kept.get(&EmptyJournalKeepReason::Recent), Some(&1));
    assert!(dir.exists());
    assert!(matches!(inspect(&dir, 0, true).unwrap(), Err(EmptyJournalKeepReason::Recent)));
}

#[cfg(unix)]
#[test]
fn symlink_transcript_preserves_external_target() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target");
    std::fs::write(&target, "").unwrap();
    let dir = root.path().join("linked");
    std::fs::create_dir(&dir).unwrap();
    std::os::unix::fs::symlink(&target, dir.join(TRANSCRIPT)).unwrap();
    let result = sweep_empty_transcript_journals(root.path(), u64::MAX, DEFAULT_MIN_IDLE_MS, &|_| false).unwrap();
    assert_eq!(result.kept.get(&EmptyJournalKeepReason::OtherContent), Some(&1));
    assert!(dir.join(TRANSCRIPT).symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read(&target).unwrap(), Vec::<u8>::new());
}

#[test]
fn removes_old_empty_journal_and_keeps_state_reflection_and_other_content() {
    let root = tempfile::tempdir().unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    let old_ms = old.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    for (name, transcript, state, extra) in [
        ("empty", "", r#"{"schema_version":"v3_assistant_steps","total_completed_steps":0}"#, None),
        ("content", "", r#"{"total_completed_steps":2}"#, None),
        ("other", "", "{}", Some("keep")),
        ("transcript", "user row", "{}", None),
    ] {
        let dir = root.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(TRANSCRIPT), transcript).unwrap();
        std::fs::write(dir.join(STATE), state).unwrap();
        if let Some(extra) = extra { std::fs::write(dir.join("notes.txt"), extra).unwrap(); }
        for path in [&dir, &dir.join(TRANSCRIPT), &dir.join(STATE)] {
            filetime::set_file_mtime(path, filetime::FileTime::from_unix_time((old_ms / 1000) as i64, 0)).unwrap();
        }
    }
    let result = sweep_empty_transcript_journals(root.path(), old_ms + 60 * 60 * 1000, DEFAULT_MIN_IDLE_MS, &|_| false).unwrap();
    assert!(result.removed.contains(&"empty".to_owned()));
    for name in ["content", "other", "transcript"] { assert!(root.path().join(name).exists()); }
}

#[test]
fn missing_transcripts_directory_is_empty_but_other_read_errors_propagate() {
    let root = tempfile::tempdir().unwrap();
    let missing = sweep_empty_transcript_journals(&root.path().join("missing"), 10_000_000, DEFAULT_MIN_IDLE_MS, &|_| false).unwrap();
    assert!(missing.removed.is_empty());
    std::fs::write(root.path().join("file"), "not directory").unwrap();
    assert!(sweep_empty_transcript_journals(&root.path().join("file"), 10_000_000, DEFAULT_MIN_IDLE_MS, &|_| false).unwrap().removed.is_empty());
}

#[test]
#[cfg(unix)]
fn unexpected_read_errors_propagate() {
    let root = tempfile::tempdir().unwrap();
    let read_error = root.path().join("read-error");
    std::os::unix::fs::symlink(&read_error, &read_error).unwrap();
    let now = std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    let result = sweep_empty_transcript_journals(&read_error, now + DEFAULT_MIN_IDLE_MS, DEFAULT_MIN_IDLE_MS, &|_| false);
    assert!(matches!(result, Err(EmptyJournalSweepError::Io(_))));
}

#[test]
fn nonwaiting_journal_lock_reports_contention_as_kept_locked() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("session");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(TRANSCRIPT), "").unwrap();
    std::fs::write(dir.join(STATE), "{}").unwrap();
    std::fs::write(dir.join(LOCK), format!("{}\n", std::process::id())).unwrap();
    let now = std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    let result = sweep_empty_transcript_journals(root.path(), now + DEFAULT_MIN_IDLE_MS, DEFAULT_MIN_IDLE_MS, &|_| false).unwrap();
    assert_eq!(result.kept.get(&EmptyJournalKeepReason::Locked), Some(&1));
    assert!(dir.exists());
}
