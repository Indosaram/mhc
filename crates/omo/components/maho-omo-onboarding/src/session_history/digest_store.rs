//! Session digest storage (upstream `digest-store.ts`).

use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use super::types::SessionDigest;

pub trait SessionDigestStore: Send + Sync {
    fn put(&self, scan_id: &str, digest: &SessionDigest) -> io::Result<()>;
    fn list(&self, scan_id: &str) -> io::Result<Vec<SessionDigest>>;
    fn drop_scan(&self, scan_id: &str) -> io::Result<()>;
}

fn valid_scan_id(scan_id: &str) -> bool {
    let bytes = scan_id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !scan_id.contains("..")
}

fn invalid_scan_id(scan_id: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "invalid scanId: {}",
            serde_json::Value::String(scan_id.to_owned())
        ),
    )
}

/// `<stateDir>/onboarding/scans/<scanId>`
pub fn scan_directory(state_dir: &Path, scan_id: &str) -> io::Result<PathBuf> {
    if !valid_scan_id(scan_id) {
        return Err(invalid_scan_id(scan_id));
    }
    Ok(state_dir.join("onboarding").join("scans").join(scan_id))
}

/// Digests held only in this process's memory: nothing reaches disk. The onboarding scan uses it, because
/// nothing may be persisted before the user consents; `drop_scan` (or letting the store go) releases them.
#[derive(Default)]
pub struct MemorySessionDigestStore {
    scans: Mutex<HashMap<String, Vec<SessionDigest>>>,
}

pub fn create_memory_session_digest_store() -> MemorySessionDigestStore {
    MemorySessionDigestStore::default()
}

impl SessionDigestStore for MemorySessionDigestStore {
    fn put(&self, scan_id: &str, digest: &SessionDigest) -> io::Result<()> {
        self.scans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(scan_id.to_owned())
            .or_default()
            .push(digest.clone());
        Ok(())
    }
    fn list(&self, scan_id: &str) -> io::Result<Vec<SessionDigest>> {
        Ok(self
            .scans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(scan_id)
            .cloned()
            .unwrap_or_default())
    }
    fn drop_scan(&self, scan_id: &str) -> io::Result<()> {
        self.scans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(scan_id);
        Ok(())
    }
}

/// Digests stay on this machine under the engine's own state directory
/// (`<stateDir>/onboarding/scans/<scanId>/`, owner-only permissions) and are never sent anywhere.
pub struct FileSessionDigestStore {
    state_dir: PathBuf,
}

pub fn create_session_digest_store(state_dir: impl Into<PathBuf>) -> FileSessionDigestStore {
    FileSessionDigestStore {
        state_dir: state_dir.into(),
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

impl SessionDigestStore for FileSessionDigestStore {
    fn put(&self, scan_id: &str, digest: &SessionDigest) -> io::Result<()> {
        let directory = scan_directory(&self.state_dir, scan_id)?;
        fs::create_dir_all(&directory)?;
        set_mode(&directory, 0o700)?;
        let file = directory.join("digests.jsonl");
        let mut handle = OpenOptions::new().create(true).append(true).open(&file)?;
        handle.write_all(format!("{}\n", digest.to_json_string()).as_bytes())?;
        set_mode(&file, 0o600)
    }
    fn list(&self, scan_id: &str) -> io::Result<Vec<SessionDigest>> {
        let file = scan_directory(&self.state_dir, scan_id)?.join("digests.jsonl");
        let contents = match fs::read_to_string(&file) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        contents
            .split('\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .and_then(|value| SessionDigest::from_json(&value))
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "malformed digest line")
                    })
            })
            .collect()
    }
    fn drop_scan(&self, scan_id: &str) -> io::Result<()> {
        match fs::remove_dir_all(scan_directory(&self.state_dir, scan_id)?) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_history::types::AgentSessionSource;

    fn digest(key: &str) -> SessionDigest {
        SessionDigest {
            session_key: key.into(),
            source: AgentSessionSource::Codex,
            provider_session_id: "p".into(),
            cwd: "/c".into(),
            title: "t".into(),
            model: None,
            started_at: "a".into(),
            last_active_at: "b".into(),
            message_count: 1,
            first_request: "r".into(),
            tools_used: vec![],
            files_changed: vec![],
            outcome: "o".into(),
            truncated: false,
        }
    }

    #[test]
    fn rejects_unsafe_scan_ids() {
        for bad in ["", "..", "a/b", "a..b", ".hidden", "a b"] {
            let error = scan_directory(Path::new("/s"), bad)
                .err()
                .map(|e| e.to_string());
            assert!(
                error.is_some_and(|m| m.starts_with("invalid scanId: ")),
                "{bad}"
            );
        }
        assert!(scan_directory(Path::new("/s"), "tui-1.2_x").is_ok());
    }

    #[test]
    fn file_store_round_trips_and_drops() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let store = create_session_digest_store(dir.path());
        assert!(store.list("scan1")?.is_empty());
        store.put("scan1", &digest("k1"))?;
        store.put("scan1", &digest("k2"))?;
        let listed = store.list("scan1")?;
        assert_eq!(
            listed
                .iter()
                .map(|d| d.session_key.as_str())
                .collect::<Vec<_>>(),
            vec!["k1", "k2"]
        );
        assert!(
            dir.path()
                .join("onboarding/scans/scan1/digests.jsonl")
                .exists()
        );
        store.drop_scan("scan1")?;
        assert!(store.list("scan1")?.is_empty());
        Ok(())
    }

    #[test]
    fn memory_store_never_touches_disk() -> Result<(), Box<dyn std::error::Error>> {
        let store = create_memory_session_digest_store();
        store.put("s", &digest("k"))?;
        assert_eq!(store.list("s")?.len(), 1);
        store.drop_scan("s")?;
        assert!(store.list("s")?.is_empty());
        Ok(())
    }
}
