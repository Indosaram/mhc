//! Ownership of isolation directories and cleanup of stale ones. Port of the MIT oh-my-pi
//! `task/isolation-ownership.ts` (owner marker with pid and process start token, liveness check);
//! the stale sweep follows its `worktree clear` rule (live owners are never touched).
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Marker file inside each isolation base directory.
pub const OWNER_FILE: &str = ".mhc-isolation-owner.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    pub pid: u32,
    pub id: String,
    /// Repository (or directory) the isolation was made from.
    pub source: PathBuf,
    /// `worktree` or `copy`.
    pub backend: String,
    #[serde(
        rename = "startToken",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub start_token: Option<String>,
}

/// An identifier of when `pid` started (Linux: field 22 of `/proc/<pid>/stat`), so a recycled pid
/// is not mistaken for the owner.
pub fn process_start_token(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = &stat[stat.rfind(')')? + 2..];
    after
        .split(' ')
        .nth(19)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}

/// Writes the marker for this process.
///
/// # Errors
/// The marker could not be written.
pub fn write_owner(base: &Path, id: &str, source: &Path, backend: &str) -> std::io::Result<()> {
    let pid = std::process::id();
    let owner = Owner {
        pid,
        id: id.to_owned(),
        source: source.to_path_buf(),
        backend: backend.to_owned(),
        start_token: process_start_token(pid),
    };
    std::fs::write(
        base.join(OWNER_FILE),
        serde_json::to_vec(&owner).unwrap_or_default(),
    )
}

pub fn read_owner(base: &Path) -> Option<Owner> {
    serde_json::from_slice(&std::fs::read(base.join(OWNER_FILE)).ok()?).ok()
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
        Ok(()) | Err(nix::errno::Errno::EPERM) => true,
        Err(_) => false,
    }
}

/// No signal 0 off Unix: the workspace's `tasklist` probe (an unknown pid is reported dead).
#[cfg(not(unix))]
fn pid_alive(pid: u32) -> bool {
    maho_tool_child::group_ownership::process_is_live(pid)
}

/// Whether the marker in `base` names a process that is still running (unknown = alive).
pub fn has_live_owner(base: &Path) -> bool {
    let Some(owner) = read_owner(base) else {
        return false;
    };
    if owner.pid == 0 || !pid_alive(owner.pid) {
        return false;
    }
    match (&owner.start_token, process_start_token(owner.pid)) {
        (Some(recorded), Some(current)) => *recorded == current,
        _ => true,
    }
}

fn recently_modified(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.elapsed().ok())
        .is_none_or(|age| age < std::time::Duration::from_secs(600))
}

/// Removes every isolation under `root` whose owner is gone; retained isolations
/// (`*.retained-*`) only when `include_retained`. Returns the removed directories.
pub fn clear_stale(root: &Path, include_retained: bool) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return removed;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if !name.starts_with('t') || !path.is_dir() {
            continue;
        }
        if name.contains(".retained-") && !include_retained {
            continue;
        }
        if has_live_owner(&path) {
            continue;
        }
        let owner = read_owner(&path);
        // A directory without a marker may be one being created right now (the marker is
        // written just after the directory); leave young ones alone.
        if owner.is_none() && recently_modified(&path) {
            continue;
        }
        let merged = path.join(crate::MERGED_DIR);
        if owner.as_ref().is_some_and(|o| o.backend == "worktree") {
            let _ = maho_git::Git::new(&merged).run(&["worktree", "remove", "--force", "."]);
        }
        if crate::tree::remove_existing(&path).is_ok() {
            if let Some(owner) = owner.filter(|o| o.backend == "worktree") {
                let _ = maho_git::Git::new(&owner.source).run(&["worktree", "prune"]);
            }
            removed.push(path);
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_liveness_and_sweep() {
        let root = tempfile::tempdir().expect("tempdir");
        let live = root.path().join("tlive");
        let dead = root.path().join("tdead");
        let kept = root.path().join("tdead.retained-1-2");
        for dir in [&live, &dead, &kept] {
            std::fs::create_dir_all(dir.join("m")).expect("mkdir");
        }
        write_owner(&live, "a", root.path(), "copy").expect("owner");
        assert!(has_live_owner(&live));
        let dead_owner = Owner {
            pid: u32::MAX - 7,
            id: "b".into(),
            source: root.path().to_path_buf(),
            backend: "copy".into(),
            start_token: None,
        };
        std::fs::write(
            dead.join(OWNER_FILE),
            serde_json::to_vec(&dead_owner).expect("json"),
        )
        .expect("write");
        std::fs::write(
            kept.join(OWNER_FILE),
            serde_json::to_vec(&dead_owner).expect("json"),
        )
        .expect("write");
        assert!(!has_live_owner(&dead));
        let removed = clear_stale(root.path(), false);
        assert_eq!(removed, vec![dead.clone()]);
        assert!(live.exists() && kept.exists() && !dead.exists());
        assert_eq!(clear_stale(root.path(), true), vec![kept.clone()]);
        // A fresh directory without a marker (being created) is left alone.
        let fresh = root.path().join("tfresh");
        std::fs::create_dir_all(&fresh).expect("mkdir");
        assert!(clear_stale(root.path(), true).is_empty());
        assert!(fresh.exists());
    }
}
