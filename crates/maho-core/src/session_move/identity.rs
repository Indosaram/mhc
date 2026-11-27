//! Port of senpi packages/coding-agent/src/core/session-move/identity.ts.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::paths::{PathInputOptions, resolve_path};
use crate::repository_identity::REPOSITORY_IDENTITY_ENTRY_TYPE;
use crate::session_move::bundle::{RepositoryIdentity, parse_repository_identity};

const SCAN_LIMIT_BYTES: usize = 1024 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 1024 * 1024;
const CHUNK_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("No session file of this device is at that path, or it cannot be read; nothing was read.")]
pub struct MoveIdentityUnreadableError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "This session file is larger than the {SCAN_LIMIT_BYTES} bytes this read scans, so its recorded repository was not read. The move can still be sent without the check."
)]
pub struct MoveIdentityTooLargeError;

#[derive(Debug, thiserror::Error)]
pub enum MoveIdentityError {
    #[error(transparent)]
    Unreadable(#[from] MoveIdentityUnreadableError),
    #[error(transparent)]
    TooLarge(#[from] MoveIdentityTooLargeError),
}

fn is_session_header(line: &[u8]) -> bool {
    let Ok(val) = serde_json::from_slice::<Value>(line) else {
        return false;
    };
    val.get("type").and_then(Value::as_str) == Some("session")
        && val.get("id").and_then(Value::as_str).is_some()
}

fn identity_in(line: &[u8], marker: &[u8]) -> Option<RepositoryIdentity> {
    if !line.windows(marker.len()).any(|w| w == marker) {
        return None;
    }
    let val = serde_json::from_slice::<Value>(line).ok()?;
    if val.get("type").and_then(Value::as_str) != Some("custom")
        || val.get("customType").and_then(Value::as_str) != Some(REPOSITORY_IDENTITY_ENTRY_TYPE)
    {
        return None;
    }
    val.get("data").and_then(parse_repository_identity)
}

fn scan_file(mut file: std::fs::File) -> Result<Option<RepositoryIdentity>, MoveIdentityError> {
    let marker = format!("\"{REPOSITORY_IDENTITY_ENTRY_TYPE}\"");
    let marker_bytes = marker.as_bytes();
    let mut chunk = vec![0u8; CHUNK_BYTES];
    let mut carry: Vec<u8> = Vec::new();
    let mut dropping = false;
    let mut header = false;
    let mut recorded: Option<RepositoryIdentity> = None;
    let mut position = 0usize;

    loop {
        let bytes_read = file
            .read(&mut chunk)
            .map_err(|_| MoveIdentityUnreadableError)?;
        if bytes_read == 0 {
            break;
        }
        position = position.saturating_add(bytes_read);
        if position > SCAN_LIMIT_BYTES {
            return Err(MoveIdentityTooLargeError.into());
        }
        let data = &chunk[..bytes_read];
        let mut start = 0usize;
        while let Some(rel_nl) = data[start..].iter().position(|&b| b == b'\n') {
            let nl = start + rel_nl;
            let piece = &data[start..nl];
            start = nl + 1;
            if dropping {
                dropping = false;
                carry.clear();
                if !header {
                    return Err(MoveIdentityUnreadableError.into());
                }
                continue;
            }
            if carry.is_empty() {
                take_identity_line(piece, &mut header, &mut recorded, marker_bytes)?;
            } else {
                carry.extend_from_slice(piece);
                take_identity_line(&carry, &mut header, &mut recorded, marker_bytes)?;
                carry.clear();
            }
        }
        let rest = &data[start..];
        if dropping || carry.len().saturating_add(rest.len()) > MAX_LINE_BYTES {
            carry.clear();
            dropping = true;
        } else if !rest.is_empty() {
            carry.extend_from_slice(rest);
        }
    }

    if !carry.is_empty() && !dropping {
        take_identity_line(&carry, &mut header, &mut recorded, marker_bytes)?;
    }
    if !header {
        return Err(MoveIdentityUnreadableError.into());
    }
    Ok(recorded)
}

fn take_identity_line(
    line: &[u8],
    header: &mut bool,
    recorded: &mut Option<RepositoryIdentity>,
    marker_bytes: &[u8],
) -> Result<(), MoveIdentityError> {
    if *header {
        if let Some(id) = identity_in(line, marker_bytes) {
            *recorded = Some(id);
        }
    } else if !line.is_empty() {
        if !is_session_header(line) {
            return Err(MoveIdentityUnreadableError.into());
        }
        *header = true;
    }
    Ok(())
}

fn open_nofollow_nonblock(file: &Path) -> Result<std::fs::File, MoveIdentityUnreadableError> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NOFOLLOW | O_NONBLOCK (Linux/macOS standard values; use nix flags):
        let flags = nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK;
        opts.custom_flags(flags.bits());
    }
    opts.open(file).map_err(|_| MoveIdentityUnreadableError)
}

fn session_file_inside(
    sessions_root: &Path,
    session_path: &Path,
) -> Result<PathBuf, MoveIdentityUnreadableError> {
    let root_str = resolve_path(
        &sessions_root.to_string_lossy(),
        &sessions_root.to_string_lossy(),
        &PathInputOptions::default(),
    );
    let file_str = resolve_path(
        &session_path.to_string_lossy(),
        &session_path.to_string_lossy(),
        &PathInputOptions::default(),
    );
    let root = std::fs::canonicalize(root_str).map_err(|_| MoveIdentityUnreadableError)?;
    let file = std::fs::canonicalize(file_str).map_err(|_| MoveIdentityUnreadableError)?;
    let is_jsonl = file.extension().and_then(|e| e.to_str()) == Some("jsonl");
    let parent_parent = file.parent().and_then(|p| p.parent());
    if !is_jsonl || parent_parent != Some(root.as_path()) {
        return Err(MoveIdentityUnreadableError);
    }
    Ok(file)
}

pub async fn read_session_repository_identity(
    sessions_root: &Path,
    session_path: &Path,
) -> Result<Option<RepositoryIdentity>, MoveIdentityError> {
    let sessions_root = sessions_root.to_path_buf();
    let session_path = session_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let file_path = session_file_inside(&sessions_root, &session_path)?;
        let file = open_nofollow_nonblock(&file_path)?;
        let meta = file.metadata().map_err(|_| MoveIdentityUnreadableError)?;
        if !meta.is_file() {
            return Err(MoveIdentityUnreadableError.into());
        }
        scan_file(file)
    })
    .await
    .map_err(|_| MoveIdentityUnreadableError)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn reads_identity_inside_sessions_root_and_refuses_outside_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_root = tmp.path().join("sessions");
        let project = sessions_root.join("proj");
        std::fs::create_dir_all(&project).expect("mkdir");
        let session_file = project.join("s1.jsonl");
        let lines = [
            json!({"type": "session", "id": "s1"}).to_string(),
            json!({"type": "custom", "customType": "repository-identity", "data": {"rootCommits": ["a".repeat(40)]}}).to_string(),
        ];
        std::fs::write(&session_file, format!("{}\n", lines.join("\n"))).expect("write");

        let id = read_session_repository_identity(&sessions_root, &session_file)
            .await
            .expect("ok")
            .expect("some");
        assert_eq!(id.root_commits, vec!["a".repeat(40)]);

        let outside = tmp.path().join("outside.jsonl");
        std::fs::write(&outside, lines.join("\n")).expect("write");
        assert!(
            read_session_repository_identity(&sessions_root, &outside)
                .await
                .is_err()
        );
    }
}
