//! Opportunistic GC of `senpi-bind-XXXXXX` alias directories a killed host left in the temp roots
//! (senpi#5404 review LOW 3).
//!
//! An alias is removed only when it belongs to this user, is a 0700 directory older than 60 s,
//! holds exactly its `d` link and its `.owner` record, and that record's process is provably dead.
//! An unreadable owner identity counts as alive. Removal unlinks the two entries and then removes
//! the directory, so it never recurses and fails if anything else appeared.

use crate::host_ensure_stop::{DaemonPidFile, PidFileOwnership, resolve_pid_file_ownership};
use crate::host_reservations::read_process_start_time;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const ALIAS_PREFIX: &str = "senpi-bind-";
pub const ALIAS_LINK: &str = "d";
pub const ALIAS_OWNER: &str = ".owner";
const MIN_AGE: Duration = Duration::from_secs(60);
const MAX_ALIASES_PER_ROOT: usize = 64;

/// The roots a long-path bind may place its alias in.
pub fn bind_alias_roots() -> Vec<PathBuf> {
    vec![std::env::temp_dir(), PathBuf::from("/tmp")]
}

fn is_alias_name(name: &str) -> bool {
    let Some(suffix) = name.strip_prefix(ALIAS_PREFIX) else {
        return false;
    };
    suffix.len() == 6 && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub fn reap_orphaned_bind_aliases() {
    let unique: BTreeSet<PathBuf> = bind_alias_roots().into_iter().collect();
    for root in unique {
        reap_orphaned_bind_aliases_in(&root);
    }
}

pub fn reap_orphaned_bind_aliases_in(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_alias_name(&name) {
            candidates.push(entry.path());
            if candidates.len() >= MAX_ALIASES_PER_ROOT {
                break;
            }
        }
    }
    for dir in candidates {
        let _ = reap_if_orphaned(&dir);
    }
}

fn reap_if_orphaned(dir: &Path) -> std::io::Result<()> {
    let entry = std::fs::symlink_metadata(dir)?;
    if !entry.is_dir() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if entry.uid() != rustix::process::getuid().as_raw() {
            return Ok(());
        }
        if (entry.mode() & 0o777) != 0o700 {
            return Ok(());
        }
    }
    let modified = entry.modified()?;
    if SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age < MIN_AGE)
    {
        return Ok(());
    }
    let mut contents: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    contents.sort();
    if contents != [ALIAS_OWNER, ALIAS_LINK] {
        return Ok(());
    }
    let link_path = dir.join(ALIAS_LINK);
    if !std::fs::symlink_metadata(&link_path)?
        .file_type()
        .is_symlink()
    {
        return Ok(());
    }
    let owner_path = dir.join(ALIAS_OWNER);
    if !owner_is_dead(&owner_path) {
        return Ok(());
    }
    std::fs::remove_file(&link_path)?;
    std::fs::remove_file(&owner_path)?;
    std::fs::remove_dir(dir)
}

fn owner_is_dead(file: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(file) else {
        return false;
    };
    let Ok(owner) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    let Some(obj) = owner.as_object() else {
        return false;
    };
    let Some(pid) = obj
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|p| u32::try_from(p).ok())
        .filter(|p| *p > 0)
    else {
        return false;
    };
    let Some(start) = obj
        .get("processStartTime")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return false;
    };
    let pid_file = DaemonPidFile {
        pid,
        process_start_time: Some(start.to_owned()),
    };
    resolve_pid_file_ownership(&pid_file, read_process_start_time) == PidFileOwnership::Gone
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn make_old(path: &Path) {
        let old = SystemTime::now() - Duration::from_secs(600);
        let times = std::fs::FileTimes::new()
            .set_modified(old)
            .set_accessed(old);
        if let Ok(dir) = std::fs::File::open(path) {
            let _ = dir.set_times(times);
        }
    }

    fn make_alias(
        root: &Path,
        name: &str,
        target: &Path,
        owner_json: Option<&str>,
        old: bool,
    ) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir(&dir).expect("mkdir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        if let Some(owner) = owner_json {
            std::fs::write(dir.join(ALIAS_OWNER), owner).expect("write owner");
        }
        std::os::unix::fs::symlink(target, dir.join(ALIAS_LINK)).expect("symlink");
        if old {
            make_old(&dir);
        }
        dir
    }

    #[test]
    fn removes_old_dead_alias_without_touching_target_or_live_or_young_or_malformed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();

        // Outside target preserved when dead alias is removed.
        let outside = root.join("outside");
        std::fs::create_dir(&outside).expect("outside");
        std::fs::write(outside.join("keep"), "x").expect("keep");
        let dead_owner = "{\"pid\":2147483000,\"processStartTime\":\"1\"}";
        make_alias(root, "senpi-bind-AbC123", &outside, Some(dead_owner), true);

        // Live owner (this test process) preserved even if target missing.
        let own = std::process::id();
        let own_start = read_process_start_time(own).expect("own start");
        let live_owner = serde_json::json!({"pid": own, "processStartTime": own_start}).to_string();
        make_alias(
            root,
            "senpi-bind-LiVe12",
            &root.join("moved.bind-0022"),
            Some(&live_owner),
            true,
        );

        // Young dead alias preserved.
        make_alias(
            root,
            "senpi-bind-YoUnG1",
            &root.join("gone.bind-0033"),
            Some(dead_owner),
            false,
        );

        // Bad name, missing owner, extra file, regular file all preserved.
        make_alias(
            root,
            "senpi-bind-toolong7",
            &root.join("g1"),
            Some(dead_owner),
            true,
        );
        make_alias(root, "senpi-bind-NoOwN1", &root.join("g2"), None, true);
        let extra = make_alias(
            root,
            "senpi-bind-ExTra1",
            &root.join("g3"),
            Some(dead_owner),
            true,
        );
        std::fs::write(extra.join("other"), "x").expect("extra");
        make_old(&extra);
        std::fs::write(root.join("senpi-bind-FiLe12"), "x").expect("file");

        reap_orphaned_bind_aliases_in(root);

        assert!(!root.join("senpi-bind-AbC123").exists());
        assert!(outside.join("keep").exists());
        assert!(root.join("senpi-bind-LiVe12").exists());
        assert!(root.join("senpi-bind-YoUnG1").exists());
        assert!(root.join("senpi-bind-toolong7").exists());
        assert!(root.join("senpi-bind-NoOwN1").exists());
        assert!(root.join("senpi-bind-ExTra1").exists());
        assert!(root.join("senpi-bind-FiLe12").exists());
    }
}
