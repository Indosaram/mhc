//! Opportunistic GC of other installs' leftover `senpi-rpc-host-internal-*` directories in the tmpdir and in the
//! `/tmp` fallback root (senpi `host-internal-dir-reaper.ts`, senpi#3487).
//!
//! Safe without any lock: a directory is removed only when older than 60 s, holding nothing but its `.owner`
//! record, and owned by a provably dead process.
use crate::host_lifecycle::{INTERNAL_HOST_DIR_PREFIX, internal_host_dir_roots};
use std::path::Path;

const MIN_AGE_MS: u64 = 60_000;

/// Reaps every root the internal hop directories may live in.
pub fn reap_orphaned_internal_host_dirs() {
    for root in internal_host_dir_roots() {
        reap_orphaned_internal_host_dirs_in(&root, now_ms(), &owner_is_gone);
    }
}
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}
/// Provably dead: the pid is gone, or runs with another start time. A live pid whose start time cannot be read
/// is assumed alive rather than stealing its lock.
fn owner_is_gone(pid: u32, recorded: &str) -> bool {
    if !crate::host_reservations::process_is_live(pid) {
        return true;
    }
    crate::host_reservations::read_process_start_time(pid)
        .is_some_and(|current| current != recorded)
}
/// One root, with the clock and the liveness proof injected so the decision is deterministic in tests.
pub fn reap_orphaned_internal_host_dirs_in(
    root: &Path,
    now: u64,
    gone: &dyn Fn(u32, &str) -> bool,
) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir())
            || !entry
                .file_name()
                .to_string_lossy()
                .starts_with(INTERNAL_HOST_DIR_PREFIX)
        {
            continue;
        }
        let dir = entry.path();
        let Some(owner) = std::fs::read_to_string(dir.join(".owner"))
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        else {
            continue;
        };
        let (Some(pid), Some(start), Some(created)) = (
            owner["pid"]
                .as_u64()
                .and_then(|pid| u32::try_from(pid).ok()),
            owner["processStartTime"]
                .as_str()
                .filter(|start| !start.is_empty()),
            owner["createdAt"].as_u64(),
        ) else {
            continue;
        };
        if created >= now.saturating_sub(MIN_AGE_MS) {
            continue;
        }
        if std::fs::read_dir(&dir).is_ok_and(|inner| inner.count() == 1) && gone(pid, start) {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn make(root: &Path, name: &str, created: u64, extra: bool) -> std::path::PathBuf {
        let dir = root.join(format!("{INTERNAL_HOST_DIR_PREFIX}{name}"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join(".owner"),
            serde_json::json!({"pid":7,"processStartTime":"5","createdAt":created}).to_string(),
        )
        .unwrap();
        if extra {
            std::fs::write(dir.join("host.sock"), "").unwrap();
        }
        dir
    }
    #[test]
    fn only_old_owner_only_dirs_of_provably_dead_owners_are_removed() {
        let temp = tempfile::tempdir().unwrap();
        let now = 1_000_000;
        let old = make(temp.path(), "old", now - 120_000, false);
        let young = make(temp.path(), "young", now - 1_000, false);
        let busy = make(temp.path(), "busy", now - 120_000, true);
        let alive = make(temp.path(), "alive", now - 120_000, false);
        let stranger = temp.path().join("unrelated");
        std::fs::create_dir(&stranger).unwrap();
        std::fs::write(
            alive.join(".owner"),
            serde_json::json!({"pid":8,"processStartTime":"5","createdAt":now-120_000}).to_string(),
        )
        .unwrap();
        reap_orphaned_internal_host_dirs_in(temp.path(), now, &|pid, _| pid == 7);
        assert!(!old.exists());
        assert!(young.exists());
        assert!(busy.exists());
        assert!(alive.exists());
        assert!(stranger.exists());
    }
    #[test]
    fn an_owner_record_without_a_start_time_is_never_reaped() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(format!("{INTERNAL_HOST_DIR_PREFIX}x"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join(".owner"),
            r#"{"pid":7,"processStartTime":"","createdAt":1}"#,
        )
        .unwrap();
        reap_orphaned_internal_host_dirs_in(temp.path(), 10_000_000, &|_, _| true);
        assert!(dir.exists());
    }
    #[cfg(unix)]
    #[test]
    fn the_real_liveness_proof_keeps_this_process_and_releases_a_dead_pid() {
        let own = std::process::id();
        let start = crate::host_reservations::read_process_start_time(own).unwrap_or_default();
        assert!(!owner_is_gone(own, &start));
        assert!(
            owner_is_gone(own, "not-the-start-time")
                || crate::host_reservations::read_process_start_time(own).is_none()
        );
        assert!(owner_is_gone(u32::MAX, "1"));
    }
}
