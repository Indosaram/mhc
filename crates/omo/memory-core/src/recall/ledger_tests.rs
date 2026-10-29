use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::{
    RecallLedger, RecallSurfacedEntry, recall_ledger_disk_reads, sanitize_session_filename,
};

fn entry(path: &str, hash: &str) -> RecallSurfacedEntry {
    RecallSurfacedEntry {
        path: path.to_string(),
        hash: hash.to_string(),
    }
}

fn names(dir: &TempDir) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir.path())
        .expect("read dir")
        .map(|entry| entry.expect("dir entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn given_a_plain_session_id_when_sanitized_then_it_survives_unchanged() {
    assert_eq!(sanitize_session_filename("sess-abc_123"), "sess-abc_123");
}

#[test]
fn given_an_unsafe_session_id_when_sanitized_then_separators_and_globs_collapse_to_dashes() {
    let sanitized = sanitize_session_filename("a:b/../c?d*e");
    assert_eq!(sanitized, "a-b-..-c-d-e");
    assert!(!sanitized.chars().any(|ch| ":/\\?*".contains(ch)));
}

#[test]
fn given_path_traversal_or_empty_input_when_sanitized_then_a_safe_fallback_name_results() {
    for input in ["..", ".", "///", ""] {
        assert_eq!(sanitize_session_filename(input), "session");
    }
}

#[test]
fn given_an_unknown_session_when_surfaced_paths_are_read_then_the_set_is_empty() {
    let dir = TempDir::new().expect("temp dir");
    let ledger = RecallLedger::new(dir.path());
    assert!(ledger.surfaced_paths("missing-session").is_empty());
}

#[test]
fn given_a_malformed_ledger_file_when_surfaced_paths_are_read_then_the_set_is_empty() {
    let dir = TempDir::new().expect("temp dir");
    fs::write(dir.path().join("broken.json"), "{not json").expect("write");
    let ledger = RecallLedger::new(dir.path());
    assert!(ledger.surfaced_paths("broken").is_empty());
}

#[test]
fn given_marked_entries_when_surfaced_paths_are_read_then_exactly_those_paths_return() {
    let dir = TempDir::new().expect("temp dir");
    let ledger = RecallLedger::new(dir.path());
    ledger
        .mark_surfaced(
            "session-1",
            &[entry("reference/a.md", "aaaa"), entry("notes/b.md", "bbbb")],
        )
        .expect("mark");
    let surfaced: BTreeSet<String> = ledger.surfaced_paths("session-1");
    assert_eq!(
        surfaced,
        BTreeSet::from(["reference/a.md".to_string(), "notes/b.md".to_string()])
    );
}

#[test]
fn given_marked_entries_when_the_file_is_inspected_then_the_versioned_shape_is_pinned() {
    let dir = TempDir::new().expect("temp dir");
    let ledger = RecallLedger::new(dir.path());
    ledger
        .mark_surfaced("session-1", &[entry("reference/a.md", "aaaa")])
        .expect("mark");

    let raw = fs::read_to_string(dir.path().join("session-1.json")).expect("read ledger");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse ledger");
    assert_eq!(parsed["version"], serde_json::json!(1));
    assert_eq!(parsed["surfaced"]["reference/a.md"]["hash"], serde_json::json!("aaaa"));
    let at = parsed["surfaced"]["reference/a.md"]["at"]
        .as_str()
        .expect("at string");
    assert!(at.len() >= 20 && at.contains('T'), "unexpected timestamp {at}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(dir.path().join("session-1.json"))
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn given_a_second_mark_on_the_same_session_when_read_then_earlier_entries_persist_and_repeats_update() {
    let dir = TempDir::new().expect("temp dir");
    let ledger = RecallLedger::new(dir.path());
    ledger
        .mark_surfaced("session-1", &[entry("reference/a.md", "first")])
        .expect("first mark");
    ledger
        .mark_surfaced(
            "session-1",
            &[entry("reference/a.md", "second"), entry("notes/b.md", "bbbb")],
        )
        .expect("second mark");

    let surfaced: BTreeSet<String> = ledger.surfaced_paths("session-1");
    assert_eq!(
        surfaced,
        BTreeSet::from(["reference/a.md".to_string(), "notes/b.md".to_string()])
    );
    let raw = fs::read_to_string(dir.path().join("session-1.json")).expect("read ledger");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse ledger");
    assert_eq!(parsed["surfaced"]["reference/a.md"]["hash"], serde_json::json!("second"));
}

#[test]
fn given_two_sessions_when_both_mark_entries_then_separate_files_hold_separate_sets() {
    let dir = TempDir::new().expect("temp dir");
    let ledger = RecallLedger::new(dir.path());
    ledger
        .mark_surfaced("session-1", &[entry("reference/a.md", "aaaa")])
        .expect("mark 1");
    ledger
        .mark_surfaced("session-2", &[entry("notes/b.md", "bbbb")])
        .expect("mark 2");

    assert_eq!(
        ledger.surfaced_paths("session-1"),
        BTreeSet::from(["reference/a.md".to_string()])
    );
    assert_eq!(
        ledger.surfaced_paths("session-2"),
        BTreeSet::from(["notes/b.md".to_string()])
    );
    assert_eq!(names(&dir), vec!["session-1.json", "session-2.json"]);
}

#[test]
fn given_an_unsafe_session_id_when_entries_are_marked_then_the_file_lands_sanitized() {
    let dir = TempDir::new().expect("temp dir");
    let ledger = RecallLedger::new(dir.path());
    ledger
        .mark_surfaced("a:b?c", &[entry("reference/a.md", "aaaa")])
        .expect("mark");

    let names = names(&dir);
    assert_eq!(names.len(), 1);
    assert!(!names[0].chars().any(|ch| ":/\\?*".contains(ch)));
    assert_eq!(
        ledger.surfaced_paths("a:b?c"),
        BTreeSet::from(["reference/a.md".to_string()])
    );
}

#[test]
fn given_no_entries_when_marking_then_nothing_is_written() {
    let dir = TempDir::new().expect("temp dir");
    let ledger = RecallLedger::new(dir.path());
    ledger.mark_surfaced("session-1", &[]).expect("mark");
    assert!(names(&dir).is_empty());
}

#[test]
fn given_an_unchanged_session_file_when_read_twice_then_the_file_is_parsed_once() {
    let dir = TempDir::new().expect("temp dir");
    let ledger = RecallLedger::new(dir.path());
    fs::write(
        dir.path().join("session-1.json"),
        "{\n  \"version\": 1,\n  \"surfaced\": {\n    \"reference/a.md\": {\n      \"hash\": \"aaaa\",\n      \"at\": \"2026-09-16T00:00:00.000Z\"\n    }\n  }\n}\n",
    )
    .expect("write");
    let before = recall_ledger_disk_reads();

    let first = ledger.surfaced_paths("session-1");
    let second = ledger.surfaced_paths("session-1");

    assert_eq!(first, BTreeSet::from(["reference/a.md".to_string()]));
    assert_eq!(second, first);
    assert_eq!(recall_ledger_disk_reads() - before, 1);
}

#[test]
fn given_this_instances_own_mark_when_surfaced_paths_are_read_then_the_write_updated_the_cache() {
    let dir = TempDir::new().expect("temp dir");
    let ledger = RecallLedger::new(dir.path());
    ledger
        .mark_surfaced("session-1", &[entry("reference/a.md", "aaaa")])
        .expect("first mark");
    let _ = ledger.surfaced_paths("session-1");

    ledger
        .mark_surfaced("session-1", &[entry("notes/b.md", "bbbb")])
        .expect("second mark");
    let before = recall_ledger_disk_reads();
    let surfaced = ledger.surfaced_paths("session-1");

    assert_eq!(
        surfaced,
        BTreeSet::from(["reference/a.md".to_string(), "notes/b.md".to_string()])
    );
    assert_eq!(recall_ledger_disk_reads() - before, 0);
}

#[test]
fn given_an_external_rewrite_of_the_session_file_when_read_then_the_new_content_wins() {
    let dir = TempDir::new().expect("temp dir");
    let ledger = RecallLedger::new(dir.path());
    ledger
        .mark_surfaced("session-1", &[entry("reference/a.md", "aaaa")])
        .expect("mark");
    assert_eq!(
        ledger.surfaced_paths("session-1"),
        BTreeSet::from(["reference/a.md".to_string()])
    );

    fs::write(
        dir.path().join("session-1.json"),
        "{\n  \"version\": 1,\n  \"surfaced\": {\n    \"reference/a.md\": {\n      \"hash\": \"aaaa\",\n      \"at\": \"2026-09-16T00:00:00.000Z\"\n    },\n    \"notes/external.md\": {\n      \"hash\": \"cccc\",\n      \"at\": \"2026-09-16T00:00:00.000Z\"\n    }\n  }\n}\n",
    )
    .expect("rewrite");

    assert_eq!(
        ledger.surfaced_paths("session-1"),
        BTreeSet::from(["reference/a.md".to_string(), "notes/external.md".to_string()])
    );
}

#[test]
fn given_a_deleted_session_file_when_read_then_the_cached_set_is_dropped() {
    let dir = TempDir::new().expect("temp dir");
    let ledger = RecallLedger::new(dir.path());
    ledger
        .mark_surfaced("session-1", &[entry("reference/a.md", "aaaa")])
        .expect("mark");
    assert_eq!(
        ledger.surfaced_paths("session-1"),
        BTreeSet::from(["reference/a.md".to_string()])
    );

    fs::remove_file(dir.path().join("session-1.json")).expect("remove");
    assert!(ledger.surfaced_paths("session-1").is_empty());
}

#[test]
fn given_a_ledger_dir_that_does_not_exist_yet_when_entries_are_marked_then_the_directory_is_created()
{
    let dir = TempDir::new().expect("temp dir");
    let ledger = RecallLedger::new(PathBuf::from(dir.path()).join("ledger"));
    ledger
        .mark_surfaced("session-1", &[entry("reference/a.md", "aaaa")])
        .expect("mark");
    assert_eq!(
        ledger.surfaced_paths("session-1"),
        BTreeSet::from(["reference/a.md".to_string()])
    );
    let mut names: Vec<String> = fs::read_dir(dir.path().join("ledger"))
        .expect("read ledger dir")
        .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["session-1.json"]);
}
