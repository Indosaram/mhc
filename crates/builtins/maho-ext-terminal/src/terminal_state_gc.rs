//! Port of senpi `terminal/terminal-state-gc.ts`: bounded GC of stale leases and empty manifests in
//! the shared per-cwd terminal state dir.
//!
//! Native lease shape: `manifest_lease` records `{pid, startedAtMs}` without the upstream token or
//! boot identity, so a lease is stale when it is unparseable or its pid is dead; a recycled pid
//! (upstream `reused`) is a process that `read_process_start_ms` says started after the recorded
//! acquisition instant (upstream's one-sided v1 rule).
//! Removal re-reads the file and unlinks only when the bytes are unchanged, the native analogue of
//! upstream `reclaimInspected` (a lease published while the sweep probed is never deleted).
use std::collections::HashSet;
use std::path::Path;
use std::time::SystemTime;

const ABANDONED_TEMP_MS: u128 = 30_000;
/// `utils/process-identity.ts` `PROCESS_START_TOLERANCE_MS`.
const START_TOLERANCE_MS: f64 = 3_000.0;

pub type AliveProbe<'a> = &'a dyn Fn(f64) -> bool;
pub type StartProbe<'a> = &'a dyn Fn(f64) -> Option<f64>;

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

fn lease_is_stale(raw: &[u8], options: &SweepOptions<'_>) -> bool {
    let Ok(record) = serde_json::from_slice::<crate::manifest_lease::LeaseRecord>(raw) else {
        return true;
    };
    if !record.pid.is_finite() || !record.started_at_ms.is_finite() {
        return true;
    }
    if record.pid == options.self_pid {
        return false;
    }
    let alive = options.is_process_alive.map_or_else(
        || crate::manifest_lease::probe_alive(record.pid).unwrap_or(true),
        |probe| probe(record.pid),
    );
    if !alive {
        return true;
    }
    let Some(read_start) = options.read_process_start_ms else {
        return false;
    };
    // Upstream's v1 rule (`classifyLease`): the native `startedAtMs` is when the lease was taken, so
    // its holder started no later than that; only a process that started afterwards is a reuse.
    read_start(record.pid)
        .is_some_and(|started| started > record.started_at_ms + START_TOLERANCE_MS)
}

fn sweep_entry(
    path: &Path,
    name: &str,
    options: &SweepOptions<'_>,
    result: &mut SweepResult,
) -> std::io::Result<()> {
    if name.ends_with(".tmp")
        || name.ends_with(".reclaim")
        || name.ends_with(".broken")
        || name.ends_with(".lock")
    {
        // A temp file is published within milliseconds of being written; one this old was
        // abandoned by a crash. The native lease takes no reclaim lock, so a `.lock` here is a
        // leftover from an older generation and follows the same age rule.
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
        if lease_is_stale(&raw, options)
            && std::fs::read(path).ok().as_deref() == Some(raw.as_slice())
        {
            unlink_if_present(path)?;
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
        let result = sweep_terminal_state_dir(
            dir.path(),
            &SweepOptions {
                self_pid: 13.0,
                keep: &keep,
                limit: None,
                is_process_alive: Some(&alive),
                read_process_start_ms: None,
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
}
