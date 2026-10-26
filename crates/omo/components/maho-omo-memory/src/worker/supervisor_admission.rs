//! Machine-wide admission for a reflection supervisor (`worker/supervisor-admission.ts`).
//!
//! A supervisor publishes itself as queued, waits for one of the machine's reflection-run slots, and
//! only then rebases both durable deadlines by the queue duration. No child exists until the launch
//! manifest and the ledger agree, so a crash inside this routine leaves `queued: true` and a dead
//! owner that the next reconciliation can settle.

use std::path::{Path, PathBuf};

use memory_core::identity::layout::resolve_memory_root;
use memory_core::locks::{
    ReflectionRunLease, ReflectionRunLeaseOptions, acquire_reflection_run_lease,
};

use super::run_artifacts::{
    RunLaunchManifest, read_run_json, update_run_ledger, write_run_json_atomic,
};
use super::supervisor_process_identity::{get_supervisor_process_start, read_supervisor_clock_now};

fn wall_now() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64
}

/// The admitted supervisor: the rebased manifest and the slot it must keep until its child is done.
pub struct SupervisorAdmission {
    pub manifest: RunLaunchManifest,
    pub lease: ReflectionRunLease,
}

impl SupervisorAdmission {
    /// Keeps the slot occupied by the child so the supervisor may die without freeing capacity.
    pub fn retain_for_child(&self, pid: u32) -> Result<(), String> {
        let process_start = get_supervisor_process_start(pid);
        self.lease
            .retain_for(pid, process_start.as_deref())
            .map_err(|error| error.to_string())
    }
}

/// Admits the supervisor for `run_dir`, rebasing the deadlines by the time spent queued.
pub fn admit_supervisor(run_dir: &Path) -> Result<SupervisorAdmission, String> {
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    admit_supervisor_with_clock(run_dir, &|| read_supervisor_clock_now(&env, wall_now))
}

/// `now` is the supervisor clock, so a test drives the queue duration without waiting.
pub fn admit_supervisor_with_clock(
    run_dir: &Path,
    now: &dyn Fn() -> f64,
) -> Result<SupervisorAdmission, String> {
    let launch_path = run_dir.join("launch.json");
    let ledger_path = run_dir.join("ledger.json");
    let initial: RunLaunchManifest =
        read_run_json(&launch_path).map_err(|error| error.to_string())?;
    let queued_at = initial.queued_at.unwrap_or_else(now);
    update_run_ledger(
        &ledger_path,
        &serde_json::Map::from_iter([
            ("launching".into(), false.into()),
            ("queued".into(), true.into()),
            ("pid".into(), std::process::id().into()),
            (
                "processStart".into(),
                get_supervisor_process_start(std::process::id()).into(),
            ),
        ]),
    )
    .map_err(|error| error.to_string())?;
    let root = match &initial.memory_root {
        Some(root) => PathBuf::from(root),
        None => resolve_memory_root(&initial.env, Path::new(&initial.cwd)),
    };
    let locks = root.join("runtime").join("locks");
    let lease = acquire_reflection_run_lease(
        &locks,
        &ReflectionRunLeaseOptions {
            max_concurrent: initial.max_concurrent_runs,
            ..Default::default()
        },
    )
    .map_err(|error| error.to_string())?;
    let admitted = (|| -> Result<SupervisorAdmission, String> {
        let admitted_at = now();
        if !queued_at.is_finite() || !admitted_at.is_finite() {
            return Err("unreadable reflection admission clock".into());
        }
        let hard_deadline_at = initial.hard_deadline_at + (admitted_at - queued_at).max(0.0);
        let manifest = RunLaunchManifest {
            hard_deadline_at,
            ..initial.clone()
        };
        // No child exists until BOTH files agree. A crash here leaves queued=true and a dead owner.
        write_run_json_atomic(&launch_path, &manifest, 0o600).map_err(|error| error.to_string())?;
        update_run_ledger(
            &ledger_path,
            &serde_json::Map::from_iter([
                ("queued".into(), false.into()),
                ("hardDeadlineAt".into(), hard_deadline_at.into()),
                (
                    "deadlineAt".into(),
                    (hard_deadline_at + initial.termination_grace_ms).into(),
                ),
            ]),
        )
        .map_err(|error| error.to_string())?;
        Ok(SupervisorAdmission {
            manifest,
            lease: lease.clone(),
        })
    })();
    if admitted.is_err() {
        lease.release();
    }
    admitted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::run_artifacts::RunKind;

    fn seed(root: &Path, memory_root: &Path, queued_at: f64, max_concurrent_runs: Option<usize>) {
        write_run_json_atomic(
            &root.join("launch.json"),
            &RunLaunchManifest {
                version: 1,
                run_id: "run".into(),
                attempt: 1,
                next_attempt: None,
                kind: RunKind::Reflection,
                command: "child".into(),
                args: vec![],
                cwd: root.to_string_lossy().into_owned(),
                env: Default::default(),
                hard_deadline_at: queued_at + 10_000.0,
                queued_at: Some(queued_at),
                memory_root: Some(memory_root.to_string_lossy().into_owned()),
                max_concurrent_runs,
                termination_grace_ms: 1_000.0,
                max_output_bytes: 1024,
                stdout_path: root.join("stdout").to_string_lossy().into_owned(),
                stderr_path: root.join("stderr").to_string_lossy().into_owned(),
            },
            0o600,
        )
        .unwrap();
        write_run_json_atomic(
            &root.join("ledger.json"),
            &serde_json::json!({"version":1,"runId":"run","kind":"reflection","trigger":"manual","startedAt":"2026-01-01T00:00:00.000Z","hardDeadlineAt":queued_at + 10_000.0,"terminationGraceMs":1000,"deadlineAt":queued_at + 11_000.0,"mergePolicy":"auto","launching":true,"queued":true}),
            0o600,
        )
        .unwrap();
    }

    #[test]
    fn waiting_beyond_the_original_budget_rebases_both_durable_deadlines() {
        let root = tempfile::tempdir().unwrap();
        let memory_root = root.path().join("memory");
        seed(root.path(), &memory_root, 1_000.0, Some(1));
        // Twenty seconds queued: the manifest and the ledger both move by that duration.
        let admission = admit_supervisor_with_clock(root.path(), &|| 21_000.0).unwrap();
        let manifest: RunLaunchManifest = read_run_json(&root.path().join("launch.json")).unwrap();
        let ledger: serde_json::Value = read_run_json(&root.path().join("ledger.json")).unwrap();
        assert_eq!(admission.manifest.hard_deadline_at, 31_000.0);
        assert_eq!(manifest.hard_deadline_at, 31_000.0);
        assert_eq!(ledger["hardDeadlineAt"], 31_000.0);
        assert_eq!(ledger["deadlineAt"], 32_000.0);
        assert_eq!(ledger["queued"], false);
        assert_eq!(ledger["launching"], false);
        assert_eq!(ledger["startedAt"], "2026-01-01T00:00:00.000Z");
        admission.lease.release();
    }

    #[test]
    fn the_supervisor_publishes_itself_as_queued_before_it_is_admitted() {
        let root = tempfile::tempdir().unwrap();
        let memory_root = root.path().join("memory");
        seed(root.path(), &memory_root, 1_000.0, None);
        // The admission clock is read only after the queued owner is durable: observing the ledger
        // from inside that read proves the order without waiting on a second process.
        let observed = std::cell::RefCell::new(None);
        let admission = admit_supervisor_with_clock(root.path(), &|| {
            *observed.borrow_mut() =
                Some(read_run_json::<serde_json::Value>(&root.path().join("ledger.json")).unwrap());
            2_000.0
        })
        .unwrap();
        let ledger = observed.into_inner().expect("admission read the clock");
        assert_eq!(ledger["queued"], true);
        assert_eq!(ledger["launching"], false);
        assert_eq!(ledger["pid"], std::process::id());
        admission.lease.release();
    }

    #[test]
    fn an_unreadable_clock_releases_the_slot_it_took() {
        let root = tempfile::tempdir().unwrap();
        let memory_root = root.path().join("memory");
        seed(root.path(), &memory_root, 1_000.0, Some(1));
        assert_eq!(
            admit_supervisor_with_clock(root.path(), &|| f64::NAN).err(),
            Some("unreadable reflection admission clock".into())
        );
        let locks = memory_root.join("runtime").join("locks");
        let lease = memory_core::locks::acquire_reflection_run_lease(
            &locks,
            &ReflectionRunLeaseOptions::default(),
        )
        .unwrap();
        lease.release();
    }

    #[test]
    fn the_admitted_slot_names_the_child_before_the_supervisor_releases_the_gate() {
        let root = tempfile::tempdir().unwrap();
        let memory_root = root.path().join("memory");
        seed(root.path(), &memory_root, 1_000.0, Some(1));
        let admission = admit_supervisor_with_clock(root.path(), &|| 1_000.0).unwrap();
        admission.retain_for_child(std::process::id()).unwrap();
        let lock_path = memory_core::locks::reflection_run_lock_path(
            &memory_root.join("runtime").join("locks"),
            1,
        )
        .unwrap();
        let record =
            memory_core::locks::parse_lock_record(&std::fs::read_to_string(&lock_path).unwrap())
                .unwrap();
        assert_eq!(record.pid, std::process::id());
        assert!(admission.lease.release());
    }
}
