//! Port of `packages/omo-senpi/src/components/telemetry/session-summary-liveness.ts`.
//!
//! An owner is alive only while its pid is alive, the entry names the same boot id, AND the pid still names
//! the process that wrote the entry (same start time). When the start time cannot be compared (Windows, or an
//! entry written without one) a live pid with a matching entry is ambiguous, and ambiguous counts as alive,
//! like EPERM: wrongly keeping a dead owner's files costs a delay, wrongly adopting a live owner's window
//! costs a duplicate.

use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use crate::session_summary_files::{link_if_absent, remove_file, rename_if_present};
use crate::session_summary_owner::{
    OwnerRegistryInput, ProcessOwner, ProcessStartProbe, owner_entry_path,
};
use crate::session_summary_process_start::read_process_start_time;

pub type PidProbe = Arc<dyn Fn(u32) -> bool + Send + Sync>;

/// Only ESRCH (no such process) proves a pid dead; every other answer is ambiguous and counts as alive.
pub fn process_is_alive(pid: u32) -> bool {
    omo_config_core::migration::default_is_process_alive(pid)
}

pub fn create_owner_liveness(
    input: &OwnerRegistryInput,
    is_pid_alive: Option<PidProbe>,
) -> Arc<dyn Fn(&ProcessOwner) -> bool + Send + Sync> {
    let dir = input.dir.clone();
    let own = input.owner.clone();
    let is_pid_alive = is_pid_alive.unwrap_or_else(|| Arc::new(process_is_alive));
    let start_time_of: ProcessStartProbe = input
        .process_start_time
        .clone()
        .unwrap_or_else(|| Arc::new(read_process_start_time));
    Arc::new(move |owner: &ProcessOwner| {
        if owner.pid == own.pid && owner.boot_id == own.boot_id {
            return true;
        }
        let path = owner_entry_path(&dir, owner.pid);
        let text = std::fs::read_to_string(&path).ok();
        let entry = text.as_deref().and_then(parse_entry);
        let matches = entry
            .as_ref()
            .is_some_and(|entry| entry.get("bootId").and_then(Value::as_str) == Some(&owner.boot_id));
        // Only an entry this call judges may be forgotten, and never this process's own.
        let judged = matches
            .then(|| text.clone().unwrap_or_default())
            .filter(|_| owner.pid != own.pid);
        if !is_pid_alive(owner.pid) {
            return forget(&path, judged.as_deref(), &own);
        }
        let Some(entry) = entry.filter(|_| matches) else {
            return false;
        };
        let recorded = entry.get("startTime").and_then(Value::as_str);
        let current = recorded.map(|_| start_time_of(owner.pid));
        match (recorded, current) {
            (Some(recorded), Some(Some(current))) if current != recorded => {
                forget(&path, judged.as_deref(), &own)
            }
            (Some(_), Some(None)) => true,
            (Some(_), Some(Some(_))) => true,
            // An entry written without a start time is ambiguous, and ambiguous counts as alive.
            _ => true,
        }
    })
}

fn parse_entry(text: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(text).ok()?;
    value.is_object().then_some(value)
}

/// Moves the entry aside (an atomic hand-over), deletes it when it is the one judged, and otherwise links it
/// back unless an even newer entry has been written in the meantime.
fn forget(path: &Path, judged: Option<&str>, reader: &ProcessOwner) -> bool {
    let Some(judged) = judged else {
        return false;
    };
    let aside = Path::new(&format!(
        "{}.forget-{}-{}",
        path.display(),
        reader.pid,
        reader.boot_id
    ))
    .to_path_buf();
    if !rename_if_present(path, &aside) {
        return false;
    }
    let current = std::fs::read_to_string(&aside).ok();
    if current.as_deref() != Some(judged) {
        let _restored = link_if_absent(&aside, path);
    }
    remove_file(&aside);
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_summary_owner::{OwnerRegistry, owner_entry_path};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn owner(pid: u32, boot_id: &str) -> ProcessOwner {
        ProcessOwner {
            pid,
            boot_id: boot_id.into(),
        }
    }

    fn write_entry(dir: &Path, pid: u32, boot_id: &str, start: Option<&str>, heartbeat: f64) {
        let path = owner_entry_path(dir, pid);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            json!({"pid": pid, "bootId": boot_id, "startTime": start, "heartbeatAt": heartbeat})
                .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn own_owner_is_always_alive() {
        let dir = tempfile::tempdir().unwrap();
        let alive = create_owner_liveness(
            &OwnerRegistryInput {
                dir: dir.path().to_path_buf(),
                owner: owner(1, "boot-self"),
                process_start_time: None,
                now: None,
            },
            Some(Arc::new(|_| false)),
        );
        assert!(alive(&owner(1, "boot-self")));
    }

    #[test]
    fn dead_pid_is_dead_and_its_entry_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        write_entry(dir.path(), 55, "boot-dead", Some("proc:1"), 0.0);
        let alive = create_owner_liveness(
            &OwnerRegistryInput {
                dir: dir.path().to_path_buf(),
                owner: owner(1, "boot-self"),
                process_start_time: Some(Arc::new(|_| Some("proc:1".into()))),
                now: None,
            },
            Some(Arc::new(|_| false)),
        );
        assert!(!alive(&owner(55, "boot-dead")));
        assert!(!owner_entry_path(dir.path(), 55).exists());
    }

    #[test]
    fn pid_reuse_with_a_different_start_time_is_dead() {
        let dir = tempfile::tempdir().unwrap();
        write_entry(dir.path(), 66, "boot-old", Some("proc:100"), 0.0);
        let alive = create_owner_liveness(
            &OwnerRegistryInput {
                dir: dir.path().to_path_buf(),
                owner: owner(1, "boot-self"),
                process_start_time: Some(Arc::new(|_| Some("proc:200".into()))),
                now: None,
            },
            Some(Arc::new(|_| true)),
        );
        assert!(!alive(&owner(66, "boot-old")));
        assert!(!owner_entry_path(dir.path(), 66).exists());
    }

    #[test]
    fn matching_start_time_stays_alive_however_stale_the_heartbeat() {
        let dir = tempfile::tempdir().unwrap();
        write_entry(dir.path(), 77, "boot-live", Some("proc:100"), 0.0);
        let alive = create_owner_liveness(
            &OwnerRegistryInput {
                dir: dir.path().to_path_buf(),
                owner: owner(1, "boot-self"),
                process_start_time: Some(Arc::new(|_| Some("proc:100".into()))),
                now: None,
            },
            Some(Arc::new(|_| true)),
        );
        assert!(alive(&owner(77, "boot-live")));
        assert!(owner_entry_path(dir.path(), 77).exists());
    }

    #[test]
    fn unreadable_start_time_is_ambiguous_but_missing_entry_is_dead() {
        let dir = tempfile::tempdir().unwrap();
        write_entry(dir.path(), 88, "boot-win", Some("proc:100"), 0.0);
        let alive = create_owner_liveness(
            &OwnerRegistryInput {
                dir: dir.path().to_path_buf(),
                owner: owner(1, "boot-self"),
                process_start_time: Some(Arc::new(|_| None)),
                now: None,
            },
            Some(Arc::new(|_| true)),
        );
        assert!(alive(&owner(88, "boot-win")));
        // session-summary-liveness.ts requires a matching registry entry even for a live pid.
        assert!(!alive(&owner(999, "boot-unknown")));
        assert!(!owner_entry_path(dir.path(), 999).exists());
    }

    #[test]
    fn mismatched_boot_id_is_dead_but_keeps_its_entry() {
        let dir = tempfile::tempdir().unwrap();
        write_entry(dir.path(), 99, "boot-newer", Some("proc:100"), 0.0);
        let alive = create_owner_liveness(
            &OwnerRegistryInput {
                dir: dir.path().to_path_buf(),
                owner: owner(1, "boot-self"),
                process_start_time: Some(Arc::new(|_| Some("proc:100".into()))),
                now: None,
            },
            Some(Arc::new(|_| true)),
        );
        assert!(!alive(&owner(99, "boot-older")));
        assert!(owner_entry_path(dir.path(), 99).exists());
    }

    #[test]
    fn a_concurrently_replaced_entry_survives_the_forget() {
        let dir = tempfile::tempdir().unwrap();
        let pid = 123;
        write_entry(dir.path(), pid, "boot-judged", Some("proc:1"), 0.0);
        let path = owner_entry_path(dir.path(), pid);
        let judged = std::fs::read_to_string(&path).unwrap();
        // A newer process on the same pid writes its own entry after the read but before the forget.
        let alive = create_owner_liveness(
            &OwnerRegistryInput {
                dir: dir.path().to_path_buf(),
                owner: owner(1, "boot-self"),
                process_start_time: Some(Arc::new(move |_| Some("proc:2".into()))),
                now: None,
            },
            Some(Arc::new(|_| true)),
        );
        std::fs::write(&path, judged.replace("boot-judged", "boot-replacement")).unwrap();
        let replacement = std::fs::read_to_string(&path).unwrap();
        assert!(!alive(&owner(pid, "boot-judged")));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), replacement);
    }

    #[test]
    fn registry_and_liveness_agree_about_a_live_owner() {
        let dir = tempfile::tempdir().unwrap();
        let live = Arc::new(AtomicBool::new(true));
        let probes = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&probes);
        let input = OwnerRegistryInput {
            dir: dir.path().to_path_buf(),
            owner: owner(std::process::id(), "boot-self"),
            process_start_time: Some(Arc::new(move |_| {
                observed.fetch_add(1, Ordering::SeqCst);
                Some("proc:9".into())
            })),
            now: Some(Arc::new(|| 100.0)),
        };
        OwnerRegistry::new(input.clone()).register();
        let flag = Arc::clone(&live);
        let alive = create_owner_liveness(&input, Some(Arc::new(move |_| flag.load(Ordering::SeqCst))));
        assert!(alive(&owner(std::process::id(), "boot-self")));
        live.store(false, Ordering::SeqCst);
        assert!(alive(&owner(std::process::id(), "boot-self")));
    }
}
