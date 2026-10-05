use maho_server::app_server::thread_registry::{fork_from_session_file, fork_session_manager};
use serde_json::json;

fn write_source(path: &std::path::Path, records: &[serde_json::Value]) {
    std::fs::write(path, records.iter().map(|record| record.to_string()).collect::<Vec<_>>().join("\n")).expect("write source records");
}

#[test]
fn missing_source_creates_a_parent_linked_session_without_history() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("absent.jsonl");
    let manager = fork_session_manager(Some(missing.to_str().unwrap()), directory.path().to_str().unwrap(), Some(directory.path().to_str().unwrap())).unwrap();
    assert!(manager.session_file().is_some());
    let header = manager.entries().into_iter().next().unwrap();
    assert_eq!(header["type"], "session");
    assert_eq!(header["parentSession"], json!(missing.to_str().unwrap()));
    assert_eq!(manager.entries().len(), 1);
}

#[test]
fn absent_source_argument_creates_an_empty_session() {
    let directory = tempfile::tempdir().unwrap();
    let manager = fork_session_manager(None, directory.path().to_str().unwrap(), Some(directory.path().to_str().unwrap())).unwrap();
    let header = manager.entries().into_iter().next().unwrap();
    assert!(header.get("parentSession").is_none());
}

#[test]
fn existing_source_copies_every_non_header_entry_under_a_new_identity() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.jsonl");
    write_source(&source, &[
        json!({"type":"session","id":"source","version":3,"cwd":"/work","timestamp":"2020-01-01T00:00:00.000Z"}),
        json!({"type":"message","id":"u","parentId":null,"timestamp":"2020-01-01T00:00:01.000Z","message":{"role":"user","content":"hello"}}),
        json!({"type":"message","id":"a","parentId":"u","timestamp":"2020-01-01T00:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"persisted"}]}}),
    ]);
    let manager = fork_from_session_file(source.to_str().unwrap(), directory.path().to_str().unwrap(), Some(directory.path().to_str().unwrap())).unwrap();
    assert_ne!(manager.session_id(), "source");
    let path = manager.session_file().unwrap().to_owned();
    let contents = std::fs::read_to_string(&path).unwrap();
    let lines = contents.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 3);
    let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(header["type"], "session");
    assert_eq!(header["id"], manager.session_id());
    assert_eq!(header["cwd"], directory.path().to_str().unwrap());
    assert_eq!(header["parentSession"], source.to_str().unwrap());
    assert!(contents.contains("\"persisted\""));
    assert_eq!(manager.entries().len(), 3);
}

#[test]
fn empty_source_file_fails_without_creating_a_fork() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("empty.jsonl");
    std::fs::write(&source, "").unwrap();
    let error = fork_from_session_file(source.to_str().unwrap(), directory.path().to_str().unwrap(), Some(directory.path().to_str().unwrap())).unwrap_err();
    assert!(error.contains("empty or invalid"), "{error}");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn source_without_a_session_header_fails_without_creating_a_fork() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("headerless.jsonl");
    write_source(&source, &[json!({"type":"message","id":"u","message":{"role":"user","content":"hello"}})]);
    let error = fork_from_session_file(source.to_str().unwrap(), directory.path().to_str().unwrap(), Some(directory.path().to_str().unwrap())).unwrap_err();
    assert!(error.contains("no header"), "{error}");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}
