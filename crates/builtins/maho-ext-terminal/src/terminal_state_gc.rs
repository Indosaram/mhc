//! Port of senpi `terminal/terminal-state-gc.ts`: bounded GC of stale leases and empty manifests in
//! the shared per-cwd terminal state dir.
//!
//! A lease is judged by `manifest_lease::classify_lease` like an acquire: stale when unparseable,
//! dead, or reused (v2: the recorded process start does not match; v1: the process now wearing the
//! pid started after the recorded acquisition instant). Without a `read_process_start_ms` option
//! the sweep reads start instants through `manifest_lease::read_process_start_ms`, as upstream
//! defaults to `defaultReadProcessStartMs`.
//! A stale lease is removed only through `manifest_lease::reclaim_inspected`, the same locked,
//! inspected-bytes reclaim an acquire uses, so a lease another process published while the sweep
//! probed is never deleted. A `<lease>.lock` goes only once `reclaim_lock_state` calls it stale.
use crate::manifest_lease::{
    LeaseClassification, LeaseProbes, LockState, ReclaimOutcome, break_stale_lock,
    reclaim_inspected, reclaim_lock_state,
};
use std::collections::HashSet;
use std::path::Path;
use std::time::SystemTime;

const ABANDONED_TEMP_MS: u128 = 30_000;

pub type AliveProbe<'a> = &'a dyn Fn(f64) -> bool;
pub type StartProbe<'a> = &'a dyn Fn(f64) -> Option<f64>;

/// `readProcessStartMs ?? defaultReadProcessStartMs`.
fn read_start(options: &SweepOptions<'_>, pid: f64) -> Option<f64> {
    match options.read_process_start_ms {
        Some(probe) => probe(pid),
        None => crate::manifest_lease::read_process_start_ms(pid),
    }
}

pub struct SweepOptions<'a> {
    pub self_pid: f64,
    pub keep: &'a HashSet<String>,
    pub limit: Option<usize>,
    pub is_process_alive: Option<AliveProbe<'a>>,
    pub read_process_start_ms: Option<StartProbe<'a>>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepResult {
    pub examined: usize,
    pub removed_leases: usize,
    pub removed_manifests: usize,
}

fn unlink_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Upstream: `readLeaseRecord` then `classifyLease(record, options.self, probes)`; an unparseable
/// record is dead, and only a dead or reused holder is stale (our own pid never is).
fn lease_is_stale(
    raw: &[u8],
    self_pid: f64,
    probes: &mut LeaseProbes<'_>,
) -> std::io::Result<bool> {
    let Some(record) = crate::manifest_lease::read_lease_record(raw) else {
        return Ok(true);
    };
    Ok(matches!(
        crate::manifest_lease::classify_lease(&record, self_pid, probes)?,
        LeaseClassification::Dead | LeaseClassification::Reused
    ))
}

fn sweep_entry(
    path: &Path,
    name: &str,
    options: &SweepOptions<'_>,
    result: &mut SweepResult,
) -> std::io::Result<()> {
    let mut is_process_alive = |pid: f64| -> std::io::Result<bool> {
        match options.is_process_alive {
            Some(probe) => Ok(probe(pid)),
            None => crate::manifest_lease::probe_alive(pid),
        }
    };
    let mut read_process_start_ms = |pid: f64| read_start(options, pid);
    let mut probes = LeaseProbes {
        is_process_alive: &mut is_process_alive,
        read_process_start_ms: &mut read_process_start_ms,
    };
    // A reclaim lock goes only once its holder is gone; a temp file is published within
    // milliseconds of being written, so one this old was abandoned by a crash.
    if name.ends_with(".lock") {
        if let LockState::Stale(raw) = reclaim_lock_state(path, &mut probes)? {
            break_stale_lock(path, &raw)?;
        }
        return Ok(());
    }
    if name.ends_with(".tmp") || name.ends_with(".reclaim") || name.ends_with(".broken") {
        let Ok(info) = std::fs::metadata(path) else {
            return Ok(());
        };
        let age = SystemTime::now()
            .duration_since(info.modified()?)
            .map_or(0, |age| age.as_millis());
        if age > ABANDONED_TEMP_MS {
            unlink_if_present(path)?;
        }
        return Ok(());
    }
    if name.ends_with(".lease") {
        let raw = std::fs::read(path)?;
        // Removal goes through the same inspected-record reclaim as an acquire, so a lease another
        // process published while this sweep was probing is never deleted.
        if lease_is_stale(&raw, options.self_pid, &mut probes)?
            && reclaim_inspected(path, &raw, &mut probes)? == ReclaimOutcome::Removed
        {
            result.removed_leases += 1;
        }
    } else if name.ends_with(".json") {
        let parsed: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let (Some(monitors), Some(background)) = (
            parsed.get("monitors").and_then(serde_json::Value::as_array),
            parsed
                .get("backgroundSessions")
                .and_then(serde_json::Value::as_array),
        ) else {
            return Ok(());
        };
        if monitors.is_empty() && background.is_empty() {
            unlink_if_present(path)?;
            result.removed_manifests += 1;
        }
    }
    Ok(())
}

/// `sweepTerminalStateDir`: examines at most `limit` (default 500) entries; per-entry failures
/// are skipped like upstream's empty catch.
pub fn sweep_terminal_state_dir(
    dir: &Path,
    options: &SweepOptions<'_>,
) -> std::io::Result<SweepResult> {
    let mut names: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|entry| {
            entry
                .ok()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
        })
        .collect();
    names.truncate(options.limit.unwrap_or(500));
    let mut result = SweepResult {
        examined: names.len(),
        ..Default::default()
    };
    for name in &names {
        if options.keep.contains(name) {
            continue;
        }
        let _ = sweep_entry(&dir.join(name), name, options, &mut result);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::mpsc;
    use std::time::Duration;

    fn lease(dir: &Path, name: &str, pid: f64, started: f64) {
        std::fs::write(
            dir.join(name),
            serde_json::json!({"pid": pid, "startedAtMs": started}).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn reclaims_dead_and_empty_files_keeps_live_self_and_unparseable_json() {
        let dir = tempfile::tempdir().unwrap();
        lease(dir.path(), "dead.lease", 11.0, 1.0);
        lease(dir.path(), "live.lease", 12.0, 1.0);
        lease(dir.path(), "self.lease", 13.0, 1.0);
        lease(dir.path(), "kept.lease", 11.0, 1.0);
        std::fs::write(dir.path().join("broken.lease"), "{{").unwrap();
        std::fs::write(
            dir.path().join("empty.json"),
            r#"{"monitors":[],"backgroundSessions":[]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("full.json"),
            r#"{"monitors":[{}],"backgroundSessions":[]}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("bad.json"), "nope").unwrap();
        let keep = HashSet::from(["kept.lease".to_owned()]);
        let alive = |pid: f64| pid != 11.0;
        let unknown_start = |_: f64| None;
        let result = sweep_terminal_state_dir(
            dir.path(),
            &SweepOptions {
                self_pid: 13.0,
                keep: &keep,
                limit: None,
                is_process_alive: Some(&alive),
                read_process_start_ms: Some(&unknown_start),
            },
        )
        .unwrap();
        assert_eq!(
            result,
            SweepResult {
                examined: 8,
                removed_leases: 2,
                removed_manifests: 1
            }
        );
        for kept in [
            "live.lease",
            "self.lease",
            "kept.lease",
            "full.json",
            "bad.json",
        ] {
            assert!(dir.path().join(kept).exists(), "{kept}");
        }
    }

    #[test]
    fn removes_a_reused_pid_lease_and_probes_start_time_once() {
        let dir = tempfile::tempdir().unwrap();
        lease(dir.path(), "reused.lease", 21.0, 1_000.0);
        let probes = Cell::new(0);
        let start = |_: f64| {
            probes.set(probes.get() + 1);
            Some(1_000_000.0)
        };
        let alive = |_: f64| true;
        let keep = HashSet::new();
        let result = sweep_terminal_state_dir(
            dir.path(),
            &SweepOptions {
                self_pid: 1.0,
                keep: &keep,
                limit: None,
                is_process_alive: Some(&alive),
                read_process_start_ms: Some(&start),
            },
        )
        .unwrap();
        assert_eq!(result.removed_leases, 1);
        assert_eq!(probes.get(), 1);
    }

    #[test]
    fn keeps_a_live_holder_that_took_its_lease_long_after_its_process_started() {
        let dir = tempfile::tempdir().unwrap();
        // startedAtMs is the acquisition instant; the holder process started 9 s before it.
        lease(dir.path(), "live.lease", 21.0, 10_000.0);
        let start = |_: f64| Some(1_000.0);
        let alive = |_: f64| true;
        let keep = HashSet::new();
        let result = sweep_terminal_state_dir(
            dir.path(),
            &SweepOptions {
                self_pid: 1.0,
                keep: &keep,
                limit: None,
                is_process_alive: Some(&alive),
                read_process_start_ms: Some(&start),
            },
        )
        .unwrap();
        assert_eq!(result.removed_leases, 0);
        assert!(dir.path().join("live.lease").exists());
    }

    #[test]
    fn removes_a_temp_file_abandoned_by_a_crash_and_keeps_one_that_is_being_published() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("a.tmp");
        std::fs::write(&old, "x").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(SystemTime::now() - std::time::Duration::from_secs(60))
            .unwrap();
        std::fs::write(dir.path().join("b.tmp"), "x").unwrap();
        let keep = HashSet::new();
        sweep_terminal_state_dir(
            dir.path(),
            &SweepOptions {
                self_pid: 1.0,
                keep: &keep,
                limit: None,
                is_process_alive: None,
                read_process_start_ms: None,
            },
        )
        .unwrap();
        assert!(!old.exists());
        assert!(dir.path().join("b.tmp").exists());
    }

    #[test]
    fn examines_and_removes_exactly_limit_entries() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..5 {
            std::fs::write(
                dir.path().join(format!("{index}.json")),
                r#"{"monitors":[],"backgroundSessions":[]}"#,
            )
            .unwrap();
        }
        let keep = HashSet::new();
        let result = sweep_terminal_state_dir(
            dir.path(),
            &SweepOptions {
                self_pid: 1.0,
                keep: &keep,
                limit: Some(3),
                is_process_alive: None,
                read_process_start_ms: None,
            },
        )
        .unwrap();
        assert_eq!(
            result,
            SweepResult {
                examined: 3,
                removed_leases: 0,
                removed_manifests: 3
            }
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    /// The sweep judges a dead record stale, then another process replaces the lease before the
    /// sweep removes it: the fresh lease must survive.
    #[test]
    fn keeps_a_lease_replaced_between_inspection_and_removal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.lease");
        lease(dir.path(), "s.lease", 11.0, 1.0);
        let (inspected, wait_inspected) = mpsc::channel::<()>();
        let (replaced, wait_replaced) = mpsc::channel::<()>();
        let sweep = std::thread::scope(|scope| {
            let sweeper = scope.spawn(|| {
                let alive = move |pid: f64| {
                    if pid == 11.0 {
                        inspected.send(()).unwrap();
                        wait_replaced
                            .recv_timeout(Duration::from_secs(10))
                            .expect("replacement handshake");
                        return false;
                    }
                    true
                };
                let keep = HashSet::new();
                sweep_terminal_state_dir(
                    dir.path(),
                    &SweepOptions {
                        self_pid: 1.0,
                        keep: &keep,
                        limit: None,
                        is_process_alive: Some(&alive),
                        read_process_start_ms: None,
                    },
                )
            });
            wait_inspected
                .recv_timeout(Duration::from_secs(10))
                .expect("inspection handshake");
            std::fs::write(&path, br#"{"pid":12,"startedAtMs":2}"#).unwrap();
            replaced.send(()).unwrap();
            sweeper.join().unwrap()
        })
        .unwrap();
        assert_eq!(sweep.removed_leases, 0);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            br#"{"pid":12,"startedAtMs":2}"#
        );
        assert!(!dir.path().join("s.lease.lock").exists());
    }

    #[test]
    fn breaks_a_reclaim_lock_of_a_dead_holder_and_keeps_a_live_one() {
        let dir = tempfile::tempdir().unwrap();
        let holder = |pid: f64| {
            serde_json::to_vec(&crate::manifest_lease::ReclaimLockHolder {
                pid,
                boot_at_ms: crate::process_identity::process_boot_at_ms(None),
                process_started_at_ms: crate::process_identity::own_process_started_at_ms(None),
            })
            .unwrap()
        };
        std::fs::write(dir.path().join("dead.lease.lock"), holder(999_999.0)).unwrap();
        std::fs::write(
            dir.path().join("live.lease.lock"),
            holder(f64::from(std::process::id())),
        )
        .unwrap();
        let alive = |_: f64| false;
        let keep = HashSet::new();
        sweep_terminal_state_dir(
            dir.path(),
            &SweepOptions {
                self_pid: 1.0,
                keep: &keep,
                limit: None,
                is_process_alive: Some(&alive),
                read_process_start_ms: None,
            },
        )
        .unwrap();
        assert!(!dir.path().join("dead.lease.lock").exists());
        assert!(dir.path().join("live.lease.lock").exists());
    }

    /// A reclaim lock held by a live process (this one) guards its lease: the sweep backs off
    /// instead of deleting the dead-pid lease underneath it.
    #[test]
    fn keeps_a_dead_lease_while_a_live_process_holds_its_reclaim_lock() {
        let dir = tempfile::tempdir().unwrap();
        lease(dir.path(), "s.lease", 11.0, 1.0);
        let lock = dir.path().join("s.lease.lock");
        std::fs::write(
            &lock,
            serde_json::to_vec(&crate::manifest_lease::ReclaimLockHolder {
                pid: f64::from(std::process::id()),
                boot_at_ms: crate::process_identity::process_boot_at_ms(None),
                process_started_at_ms: crate::process_identity::own_process_started_at_ms(None),
            })
            .unwrap(),
        )
        .unwrap();
        let alive = |pid: f64| pid != 11.0;
        let unknown_start = |_: f64| None;
        let keep = HashSet::new();
        let result = sweep_terminal_state_dir(
            dir.path(),
            &SweepOptions {
                self_pid: 1.0,
                keep: &keep,
                limit: None,
                is_process_alive: Some(&alive),
                read_process_start_ms: Some(&unknown_start),
            },
        )
        .unwrap();
        assert_eq!(result.removed_leases, 0);
        assert!(dir.path().join("s.lease").exists());
        assert!(lock.exists());
    }

    /// Without a start probe option the sweep reads real start instants: a lease taken long before
    /// this (live) process started is a reused pid.
    #[cfg(target_os = "linux")]
    #[test]
    fn defaults_to_the_real_start_probe_when_none_is_given() {
        let dir = tempfile::tempdir().unwrap();
        lease(
            dir.path(),
            "reused.lease",
            f64::from(std::process::id()),
            1.0,
        );
        let alive = |_: f64| true;
        let keep = HashSet::new();
        let result = sweep_terminal_state_dir(
            dir.path(),
            &SweepOptions {
                self_pid: 1.0,
                keep: &keep,
                limit: None,
                is_process_alive: Some(&alive),
                read_process_start_ms: None,
            },
        )
        .unwrap();
        assert_eq!(result.removed_leases, 1);
        assert!(!dir.path().join("reused.lease").exists());
    }
}
