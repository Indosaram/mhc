//! Port of senpi packages/coding-agent/src/core/session-rebind.ts: repository classification and
//! sidecar relocation.
//!
//! `readSessionCwd` and `rebindSessionFile` live in `session_cwd.rs` (senpi-core-session lane). That
//! rebind runs without the cross-process move lock (`withSessionMoveLock` has no Rust port) and does not
//! carry sidecars; `move_session_sidecars` here is the step that lane wires in before the rename.

use std::io;
use std::path::Path;

use serde_json::Value;

use crate::repository_identity::compare_repository_identities;
use crate::session_move::bundle::{RepositoryIdentity, parse_repository_identity};
use crate::session_sidecar_store::encoded_session_id;

const REPOSITORY_IDENTITY_ENTRY_TYPE: &str = "repository-identity";

/// The last valid repository identity a session file recorded.
pub fn read_recorded_repository_identity(session_file: &str) -> Option<RepositoryIdentity> {
    crate::session_manager::load_entries_from_file(session_file)
        .iter()
        .filter(|entry| {
            entry.get("type").and_then(Value::as_str) == Some("custom")
                && entry.get("customType").and_then(Value::as_str)
                    == Some(REPOSITORY_IDENTITY_ENTRY_TYPE)
        })
        .filter_map(|entry| entry.get("data").and_then(parse_repository_identity))
        .last()
}

/// Whether `cwd` is the git repository a session was recorded in (`"same"`, `"different"` or
/// `"unknown"`). The session side prefers the live checkout at its recorded cwd and falls back to the
/// identity the session recorded, because a moved repository leaves nothing at the old path to ask.
pub async fn classify_session_repository<F, Fut>(
    session_file: &str,
    session_cwd: &str,
    cwd: &str,
    read: F,
) -> &'static str
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Option<RepositoryIdentity>>,
{
    let current = read(cwd.to_owned());
    let live = async {
        if Path::new(session_cwd).exists() {
            read(session_cwd.to_owned()).await
        } else {
            None
        }
    };
    let (current, live) = futures::join!(current, live);
    let recorded = live.or_else(|| read_recorded_repository_identity(session_file));
    compare_repository_identities(recorded.as_ref(), current.as_ref())
}

fn move_path(from: &Path, to: &Path) -> io::Result<()> {
    if to.exists() {
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            copy_recursive(from, to)?;
            if from.is_dir() {
                std::fs::remove_dir_all(from)
            } else {
                std::fs::remove_file(from)
            }
        }
        Err(error) => Err(error),
    }
}

fn copy_recursive(from: &Path, to: &Path) -> io::Result<()> {
    if !from.is_dir() {
        return std::fs::copy(from, to).map(|_| ());
    }
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
    }
    Ok(())
}

/// Per-session extension state lives at `<sessionDir>/extensions/<extension>/<encoded id>.json` (goal,
/// loop, terminal monitors) and resident blobs at `<sessionDir>/resident-blobs/<id>`; without them a
/// rebound session would lose its goal and monitors. An existing target is never overwritten.
pub fn move_session_sidecars(from_dir: &Path, to_dir: &Path, session_id: &str) -> io::Result<()> {
    let extensions_dir = from_dir.join("extensions");
    let sidecar_name = format!("{}.json", encoded_session_id(session_id));
    if extensions_dir.exists() {
        for extension in std::fs::read_dir(&extensions_dir)? {
            let extension = extension?;
            if !extension.file_type()?.is_dir() {
                continue;
            }
            let from = extension.path().join(&sidecar_name);
            if from.exists() {
                move_path(
                    &from,
                    &to_dir
                        .join("extensions")
                        .join(extension.file_name())
                        .join(&sidecar_name),
                )?;
            }
        }
    }
    let blobs = from_dir.join("resident-blobs").join(session_id);
    if blobs.exists() {
        move_path(&blobs, &to_dir.join("resident-blobs").join(session_id))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn identity(root: &str) -> RepositoryIdentity {
        RepositoryIdentity {
            root_commits: vec![root.repeat(40)],
            origin_url: None,
        }
    }

    #[tokio::test]
    async fn falls_back_to_the_recorded_identity_when_the_old_checkout_is_gone() {
        let Some(dir) = tempfile::tempdir().ok() else {
            panic!("tempdir")
        };
        let file = dir.path().join("s.jsonl");
        let lines = [
            json!({"type": "session", "id": "s"}).to_string(),
            json!({"type": "custom", "customType": REPOSITORY_IDENTITY_ENTRY_TYPE, "data": {"rootCommits": ["a".repeat(40)]}})
                .to_string(),
        ];
        assert!(std::fs::write(&file, lines.join("\n")).is_ok());
        let file = file.to_string_lossy().into_owned();
        assert_eq!(
            read_recorded_repository_identity(&file),
            Some(identity("a"))
        );
        let read = |_: String| async { Some(identity("a")) };
        assert_eq!(
            classify_session_repository(&file, "/gone/away", "/here", read).await,
            "same"
        );
        let other = |dir: String| async move { (dir == "/here").then(|| identity("b")) };
        assert_eq!(
            classify_session_repository(&file, "/gone/away", "/here", other).await,
            "different"
        );
    }

    #[test]
    fn moves_extension_sidecars_and_resident_blobs_without_overwriting() {
        let Some(dir) = tempfile::tempdir().ok() else {
            panic!("tempdir")
        };
        let (from, to) = (dir.path().join("from"), dir.path().join("to"));
        assert!(std::fs::create_dir_all(from.join("extensions/goal")).is_ok());
        assert!(std::fs::create_dir_all(from.join("resident-blobs/s-1")).is_ok());
        assert!(std::fs::write(from.join("extensions/goal/s-1.json"), "g").is_ok());
        assert!(std::fs::write(from.join("extensions/goal/other.json"), "o").is_ok());
        assert!(std::fs::write(from.join("resident-blobs/s-1/blob"), "b").is_ok());
        assert!(move_session_sidecars(&from, &to, "s-1").is_ok());
        assert_eq!(
            std::fs::read_to_string(to.join("extensions/goal/s-1.json"))
                .ok()
                .as_deref(),
            Some("g")
        );
        assert_eq!(
            std::fs::read_to_string(to.join("resident-blobs/s-1/blob"))
                .ok()
                .as_deref(),
            Some("b")
        );
        assert!(from.join("extensions/goal/other.json").exists());
        assert!(!from.join("extensions/goal/s-1.json").exists());
    }
}
