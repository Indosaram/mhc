//! `team/messaging/session-marker-index.test.ts`

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pretty_assertions::assert_eq;
use serde_json::json;

use crate::team::messaging::session_marker_index::{SessionSliceReader, create_session_marker_index};

fn temp_session_file() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().expect("tempdir");
    let file = root.path().join("session.jsonl");
    (root, file)
}

// Persisted peer-message envelope (same attribute shape the lead writes) wrapped in a session entry.
fn envelope_line(message_id: &str) -> String {
    let envelope = format!(
        "<peer_message from=\"alpha\" to=\"lead\" kind=\"message\" messageId=\"{message_id}\" timestamp=\"1\">\nready\n</peer_message>"
    );
    let entry = json!({ "type": "message", "message": { "role": "user", "content": envelope } });
    format!("{entry}\n")
}

// Counting reader: records how many bytes each slice read consumes so tests can prove the index
// reads incrementally (only appended bytes) instead of re-reading the whole session file.
fn counting_reader(counter: Arc<AtomicU64>) -> SessionSliceReader {
    Box::new(move |path: &Path, start: u64, end: u64| {
        counter.fetch_add(end - start, Ordering::SeqCst);
        let bytes = std::fs::read(path)?;
        let start = usize::try_from(start).expect("start fits");
        let end = usize::try_from(end).expect("end fits").min(bytes.len());
        Ok(String::from_utf8_lossy(&bytes[start..end]).into_owned())
    })
}

fn append(file: &Path, text: &str) {
    let mut handle = std::fs::OpenOptions::new().append(true).open(file).expect("open");
    handle.write_all(text.as_bytes()).expect("append");
}

#[test]
fn given_a_session_file_with_an_envelope_when_contains_is_queried_then_it_finds_the_message_id() {
    let (_root, file) = temp_session_file();
    std::fs::write(&file, envelope_line("id-1")).expect("write");
    let index = create_session_marker_index(None);

    assert!(index.contains(Some(&file), "id-1").expect("contains"));
    assert!(!index.contains(Some(&file), "id-missing").expect("contains"));
}

#[test]
fn given_repeated_queries_on_an_unchanged_file_when_contains_runs_again_then_no_bytes_are_re_read() {
    let (_root, file) = temp_session_file();
    std::fs::write(&file, envelope_line("id-1")).expect("write");
    let counter = Arc::new(AtomicU64::new(0));
    let index = create_session_marker_index(Some(counting_reader(Arc::clone(&counter))));

    index.contains(Some(&file), "id-1").expect("contains");
    let after_first = counter.load(Ordering::SeqCst);
    index.contains(Some(&file), "id-1").expect("contains");
    index.contains(Some(&file), "id-1").expect("contains");

    assert!(after_first > 0);
    assert_eq!(counter.load(Ordering::SeqCst), after_first);
}

#[test]
fn given_an_appended_envelope_when_contains_runs_then_only_the_appended_bytes_are_read() {
    let (_root, file) = temp_session_file();
    std::fs::write(&file, envelope_line("id-1")).expect("write");
    let counter = Arc::new(AtomicU64::new(0));
    let index = create_session_marker_index(Some(counting_reader(Arc::clone(&counter))));
    index.contains(Some(&file), "id-1").expect("contains");
    let after_first = counter.load(Ordering::SeqCst);

    let second = envelope_line("id-2");
    append(&file, &second);
    let found = index.contains(Some(&file), "id-2").expect("contains");

    assert!(found);
    assert_eq!(counter.load(Ordering::SeqCst) - after_first, second.len() as u64);
    assert!(index.contains(Some(&file), "id-1").expect("contains"));
}

#[test]
fn given_a_hidden_custom_message_with_an_envelope_when_contains_is_queried_then_it_finds_the_message_id() {
    let (_root, file) = temp_session_file();
    let message_id = "id-hidden-1";
    let content = format!("<peer_message from=\"worker\" to=\"lead\" messageId=\"{message_id}\">hello</peer_message>");
    let entry = json!({
        "type": "custom_message",
        "customType": "senpi-task:team-message",
        "display": false,
        "content": content,
    });
    std::fs::write(&file, format!("{entry}\n")).expect("write");

    let index = create_session_marker_index(None);
    assert!(index.contains(Some(&file), message_id).expect("contains"));
}

#[test]
fn given_a_missing_session_file_when_contains_runs_then_it_returns_false() {
    let index = create_session_marker_index(None);
    let missing = std::env::temp_dir().join("does-not-exist-senpi.jsonl");

    assert!(!index.contains(Some(&missing), "id-1").expect("contains"));
    assert!(!index.contains(None, "id-1").expect("contains"));
}

#[test]
fn given_a_truncated_rotated_file_when_contains_runs_then_the_index_resets_and_rescans() {
    let (_root, file) = temp_session_file();
    std::fs::write(&file, envelope_line("id-old-1") + &envelope_line("id-old-2")).expect("write");
    let index = create_session_marker_index(None);
    assert!(index.contains(Some(&file), "id-old-2").expect("contains"));

    // the file is truncated to a smaller size (rotation/reset)
    std::fs::write(&file, envelope_line("id-new")).expect("write");

    assert!(index.contains(Some(&file), "id-new").expect("contains"));
}
