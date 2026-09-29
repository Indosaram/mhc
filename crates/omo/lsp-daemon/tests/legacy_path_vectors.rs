//! Port of legacy-path-vectors.test.ts: pinned pre-migration digest contract.

use lsp_daemon::crypto::{hex_lower, sha256};
use serde_json::Value;

fn vectors() -> Value {
    serde_json::from_str(include_str!("fixtures/legacy-path-vectors.json")).unwrap()
}

fn digest16(value: &str) -> String {
    hex_lower(&sha256(value.as_bytes()))[..16].to_string()
}

fn text<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value.pointer(pointer).and_then(Value::as_str).unwrap()
}

#[test]
fn unix_vectors_retain_exact_natural_and_hashed_paths() {
    let vectors = vectors();
    let version = text(&vectors, "/version");
    let natural_dir = text(&vectors, "/naturalUnix/expected/versionDir");
    assert_eq!(
        text(&vectors, "/naturalUnix/expected/socket"),
        format!("{natural_dir}/daemon.sock")
    );
    let hashed_dir = text(&vectors, "/hashedUnix/expected/versionDir");
    assert!(hashed_dir.len() >= 100);
    assert_eq!(
        text(&vectors, "/hashedUnix/expected/socket"),
        format!(
            "{}/omo-lsp-{version}-{}.sock",
            text(&vectors, "/hashedUnix/inputs/tmpdir"),
            digest16(hashed_dir)
        )
    );
}

#[test]
fn windows_vector_retains_the_exact_named_pipe() {
    let vectors = vectors();
    let version = text(&vectors, "/version");
    let version_dir = text(&vectors, "/windowsNamedPipe/expected/versionDir");
    assert_eq!(
        version_dir,
        format!(
            "{}\\v{version}",
            text(&vectors, "/windowsNamedPipe/expected/baseDir")
        )
    );
    assert_eq!(
        text(&vectors, "/windowsNamedPipe/expected/socket"),
        format!("\\\\.\\pipe\\omo-lsp-{version}-{}", digest16(version_dir))
    );
}

#[test]
fn characterization_metadata_stays_pinned() {
    let vectors = vectors();
    assert_eq!(vectors["schemaVersion"], Value::from(1));
    assert_eq!(
        text(&vectors, "/capturedCommit"),
        "616d30bddcc7982f539602611ea2508c25360eb9"
    );
    let source = text(&vectors, "/sourceSha256");
    assert!(source.len() == 64 && source.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
    assert_eq!(
        source,
        "dd749af7e71f406acf9a8a93fe8fc7dd8850d2984e15808b920f7f54794f7bd5"
    );
}
