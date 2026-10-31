//! Port of senpi `experimental/session-catalog.ts` (pin 903b8336).
//!
//! One server-hosted Session is a directory holding `meta.json` and the worker-owned
//! `session.sqlite`. senpi's functions are async over `node:fs/promises`. These are blocking `std`
//! equivalents, and callers on an async runtime run them on a blocking pool.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const METADATA_FILE: &str = "meta.json";
const STORAGE_FILE: &str = "session.sqlite";

/// senpi `SessionCatalogMetadata`.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionCatalogMetadata {
    pub id: String,
    /// Milliseconds since the Unix epoch (a JS number).
    pub created_at: f64,
    /// The working directory the Session's agent runs in.
    pub cwd: String,
    /// The Session directory. Workers lock it and own the storage inside it.
    pub path: PathBuf,
}

/// senpi `isSessionId`: `/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/`.
pub fn is_session_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// senpi `sessionStoragePath`.
pub fn session_storage_path(metadata: &SessionCatalogMetadata) -> PathBuf {
    metadata.path.join(STORAGE_FILE)
}

/// senpi `listSessions`: every Session in the directory. Entries without valid metadata are
/// skipped, and a missing directory is empty.
pub fn list_sessions(session_dir: &Path) -> io::Result<Vec<SessionCatalogMetadata>> {
    let entries = match fs::read_dir(session_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut names = Vec::new();
    for entry in entries {
        if let Some(name) = entry?.file_name().to_str() {
            names.push(name.to_owned());
        }
    }
    Ok(names
        .iter()
        .filter(|name| is_session_id(name))
        .filter_map(|name| read_session(session_dir, name))
        .collect())
}

/// senpi `readSession`: one Session by ID, or `None` when it does not exist.
pub fn read_session(session_dir: &Path, id: &str) -> Option<SessionCatalogMetadata> {
    if !is_session_id(id) {
        return None;
    }
    let path = session_dir.join(id);
    let text = fs::read_to_string(path.join(METADATA_FILE)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let created_at = value.get("createdAt")?.as_f64()?;
    let cwd = value.get("cwd")?.as_str()?.to_owned();
    Some(SessionCatalogMetadata {
        id: id.to_owned(),
        created_at,
        cwd,
        path,
    })
}

/// senpi `createSession`: an empty Session whose worker creates the storage on first open.
pub fn create_session(
    session_dir: &Path,
    id: Option<&str>,
    cwd: &str,
) -> io::Result<SessionCatalogMetadata> {
    let id = id.map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned);
    if !is_session_id(&id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Invalid session ID: {id}"),
        ));
    }
    let path = session_dir.join(&id);
    fs::create_dir_all(session_dir)?;
    if let Err(error) = fs::create_dir(&path) {
        if error.kind() == io::ErrorKind::AlreadyExists {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("Session {id} already exists"),
            ));
        }
        return Err(error);
    }
    // `Date.now()`: whole milliseconds, serialized as an integer like JS does.
    let created_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        });
    let created_at = created_at_ms as f64;
    let body = serde_json::to_string_pretty(
        &serde_json::json!({ "createdAt": created_at_ms, "cwd": cwd }),
    )
    .map_err(io::Error::other)?;
    // senpi indents with a tab (`JSON.stringify(_, null, "\t")`).
    let body = body
        .lines()
        .map(|line| {
            let spaces = line.len() - line.trim_start_matches(' ').len();
            format!("{}{}", "\t".repeat(spaces / 2), &line[spaces..])
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(path.join(METADATA_FILE), format!("{body}\n"))?;
    Ok(SessionCatalogMetadata {
        id,
        created_at,
        cwd: cwd.to_owned(),
        path,
    })
}

/// senpi `deleteSession`. The Session's worker must be closed first.
pub fn delete_session(metadata: &SessionCatalogMetadata) -> io::Result<()> {
    match fs::remove_dir_all(&metadata.path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_follow_the_upstream_pattern() {
        assert!(is_session_id("a"));
        assert!(is_session_id("Z9._-x"));
        assert!(is_session_id(&"a".repeat(128)));
        assert!(!is_session_id(&"a".repeat(129)));
        assert!(!is_session_id(""));
        assert!(!is_session_id(".hidden"));
        assert!(!is_session_id("-flag"));
        assert!(!is_session_id("a/b"));
        assert!(!is_session_id("caf\u{e9}"));
    }

    #[test]
    fn creates_lists_reads_and_deletes_a_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");
        assert!(list_sessions(&sessions).expect("missing dir").is_empty());

        let created = create_session(&sessions, Some("s-1"), "/work/repo").expect("create");
        assert_eq!(created.path, sessions.join("s-1"));
        assert_eq!(
            session_storage_path(&created),
            sessions.join("s-1").join("session.sqlite")
        );
        let meta = fs::read_to_string(sessions.join("s-1").join("meta.json")).expect("meta");
        assert!(meta.starts_with("{\n\t\"createdAt\": "));
        assert!(meta.ends_with("\"cwd\": \"/work/repo\"\n}\n"));
        assert_eq!(read_session(&sessions, "s-1"), Some(created.clone()));

        let duplicate = create_session(&sessions, Some("s-1"), "/x").expect_err("duplicate");
        assert_eq!(duplicate.to_string(), "Session s-1 already exists");
        let invalid = create_session(&sessions, Some("../escape"), "/x").expect_err("invalid");
        assert_eq!(invalid.to_string(), "Invalid session ID: ../escape");

        delete_session(&created).expect("delete");
        delete_session(&created).expect("delete is idempotent");
        assert!(read_session(&sessions, "s-1").is_none());
    }

    #[test]
    fn listing_skips_entries_without_valid_metadata() {
        let dir = tempfile::tempdir().expect("tempdir");
        let generated = create_session(dir.path(), None, "/work").expect("create");
        assert!(uuid::Uuid::parse_str(&generated.id).is_ok());
        fs::create_dir(dir.path().join("no-meta")).expect("mkdir");
        fs::create_dir(dir.path().join("bad-meta")).expect("mkdir");
        fs::write(
            dir.path().join("bad-meta").join("meta.json"),
            "{\"createdAt\": \"x\", \"cwd\": \"/\"}",
        )
        .expect("write");
        fs::create_dir(dir.path().join(".dot")).expect("mkdir");
        let listed = list_sessions(dir.path()).expect("list");
        assert_eq!(listed, vec![generated]);
    }
}
