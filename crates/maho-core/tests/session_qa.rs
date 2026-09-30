//! QA scenarios for todo 16: a real omo session round-trip and the version-2 migration.

use std::path::PathBuf;

use maho_core::session_manager::{SessionManager, load_entries_from_file, parse_session_entries, serialize_entry};

fn sessions_root() -> PathBuf {
    std::env::var("MAHO_OMO_SESSIONS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".omo/agent/sessions"))
}

/// The newest real session file under the omo sessions root, whatever cwd directory it lives in.
fn newest_real_session() -> Option<PathBuf> {
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for directory in std::fs::read_dir(sessions_root()).ok()?.flatten() {
        let Ok(entries) = std::fs::read_dir(directory.path()) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(modified) = std::fs::metadata(&path).and_then(|metadata| metadata.modified()) else { continue };
            candidates.push((modified, path));
        }
    }
    candidates.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    candidates.into_iter().next().map(|(_, path)| path)
}

#[test]
fn round_trips_a_real_omo_session_byte_for_byte() {
    let source = newest_real_session().expect("a real omo session under ~/.omo/agent/sessions");
    let tmp = tempfile::tempdir().expect("tempdir");
    let copy = tmp.path().join(source.file_name().expect("file name"));
    std::fs::copy(&source, &copy).expect("copy");
    let original = std::fs::read(&copy).expect("read");

    let entries = load_entries_from_file(&copy.to_string_lossy());
    assert!(entries.len() > 1, "the copied session parses into header + entries");

    let manager = SessionManager::open(&copy.to_string_lossy(), Some(&tmp.path().to_string_lossy()), None, None);
    let rewritten = std::fs::read(&copy).expect("read");
    assert_eq!(
        rewritten,
        original,
        "opening a current-version session rewrites nothing: {} stays byte-identical",
        source.display()
    );
    assert_eq!(manager.session_id(), entries[0].get("id").and_then(serde_json::Value::as_str).unwrap_or_default());

    let content = String::from_utf8(original.clone()).expect("utf8");
    let reserialized: String =
        parse_session_entries(&content).iter().map(|entry| format!("{}\n", serialize_entry(entry))).collect();
    assert_eq!(reserialized.as_bytes(), original.as_slice());
}

#[test]
fn migrates_a_version_2_session_exactly_like_senpi() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("2026-09-24T01-00-00-000Z_sess.jsonl");
    let lines = [
        r#"{"type":"session","version":2,"id":"sess","timestamp":"2026-09-24T01:00:00.000Z","cwd":"/w"}"#,
        r#"{"type":"message","id":"a","parentId":null,"timestamp":"2026-09-24T01:00:01.000Z","message":{"role":"hookMessage","content":"x"}}"#,
    ];
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).expect("write");

    let manager = SessionManager::open(&path.to_string_lossy(), Some(&tmp.path().to_string_lossy()), None, None);
    let rewritten = std::fs::read_to_string(&path).expect("read");
    let expected = concat!(
        "{\"type\":\"session\",\"version\":3,\"id\":\"sess\",\"timestamp\":\"2026-09-24T01:00:00.000Z\",\"cwd\":\"/w\"}\n",
        "{\"type\":\"message\",\"id\":\"a\",\"parentId\":null,\"timestamp\":\"2026-09-24T01:00:01.000Z\",\"message\":{\"role\":\"custom\",\"content\":\"x\"}}\n",
    );
    assert_eq!(rewritten, expected, "senpi's v2 -> v3 migration: version bumped, hookMessage renamed to custom");
    assert_eq!(manager.header().expect("header")["version"], 3);
    assert_eq!(manager.entries()[0]["message"]["role"], "custom");
}
