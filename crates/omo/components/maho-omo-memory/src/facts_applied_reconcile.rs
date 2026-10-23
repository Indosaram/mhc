//! Settles `<facts>/applied/<batchId>/` envelopes an interrupted onboarding apply left behind (pin
//! `facts-applied-reconcile.ts`), with the extractor runs' rules: a live owner is left alone, a
//! malformed envelope is invisible, an envelope that never planned wrote nothing and is pruned, and a
//! planned one is finished through the retrying apply (the batch receipt makes it idempotent) and then
//! removed, whatever its terminal outcome. A busy memory-write lock keeps it for the next maintenance
//! pass. Returns true while any envelope is still owned or pending.
use std::path::Path;

use memory_core::git::GitMemoryRepo;
use memory_core::identity::resolve::MemoryIdentity;
use memory_core::locks::{ProcessLiveness, get_pid_liveness, get_process_start_identity};

use crate::facts_apply_rpc::{
    AppliedFactsRequest, WriterLock, active_applied_batches, applied_facts_dir, apply_envelope,
};
use crate::facts_batch_apply::Applied;
use crate::facts_runner_types::FactsRunLedger;
use crate::worker::run_artifacts::read_run_json;

pub fn reconcile_applied_facts_batches(
    identity: &MemoryIdentity,
    with_writer_lock: WriterLock<'_>,
    warn: &mut dyn FnMut(&str, &str, &str),
) -> bool {
    let root = applied_facts_dir(&identity.paths.facts);
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut active = false;
    for name in names {
        let run_dir = root.join(&name);
        let Ok(ledger) = read_run_json::<FactsRunLedger>(&run_dir.join("ledger.json")) else {
            continue;
        };
        if ledger.batch_id != name {
            continue;
        }
        if owner_alive(&run_dir, &ledger) {
            active = true;
            continue;
        }
        if ledger.apply_recovery.is_none() {
            let _ = std::fs::remove_dir_all(&run_dir);
            continue;
        }
        let Ok(request) = read_run_json::<AppliedFactsRequest>(&run_dir.join("request.json"))
        else {
            continue;
        };
        let outcome = match GitMemoryRepo::open(&identity.paths.repo, &identity.id)
            .map_err(|error| error.to_string())
            .and_then(|repo| {
                apply_envelope(
                    &run_dir,
                    &ledger,
                    &repo,
                    &request.records,
                    &request.people,
                    &identity.id,
                    with_writer_lock,
                )
            }) {
            Ok(None) => {
                active = true;
                continue;
            }
            Ok(Some(Applied::Committed { .. })) => "committed".to_owned(),
            Ok(Some(Applied::ParentDirty { .. })) => "parent_dirty".to_owned(),
            Ok(Some(Applied::Unchanged)) => "unchanged".to_owned(),
            Err(error) => format!("failed: {error}"),
        };
        if outcome != "committed" {
            warn(
                "facts applied batch recovery ended without a commit",
                &name,
                &outcome,
            );
        }
        let _ = std::fs::remove_dir_all(&run_dir);
    }
    active
}

fn owner_alive(run_dir: &Path, ledger: &FactsRunLedger) -> bool {
    let Some(pid) = ledger.pid else { return false };
    if pid == std::process::id() {
        return active_applied_batches()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(run_dir);
    }
    match get_pid_liveness(pid) {
        ProcessLiveness::Dead => false,
        ProcessLiveness::Unknown => true,
        ProcessLiveness::Alive => match ledger.process_start.as_deref() {
            None => true,
            Some(recorded) => {
                get_process_start_identity(pid).is_none_or(|actual| actual == recorded)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> (tempfile::TempDir, MemoryIdentity) {
        let dir = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(dir.path(), "agent");
        (
            dir,
            MemoryIdentity {
                id: "agent".into(),
                safe_slug: "agent".into(),
                paths,
            },
        )
    }

    fn envelope(
        identity: &MemoryIdentity,
        name: &str,
        batch: &str,
        pid: Option<u32>,
    ) -> std::path::PathBuf {
        let dir = applied_facts_dir(&identity.paths.facts).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let mut ledger = serde_json::json!({"version": 1, "runId": "run", "kind": "facts", "startedAt": "now", "hardDeadlineAt": 1, "terminationGraceMs": 0, "deadlineAt": 1, "batchId": batch, "queued": []});
        if let Some(pid) = pid {
            ledger["pid"] = pid.into();
        }
        std::fs::write(
            dir.join("ledger.json"),
            serde_json::to_vec(&ledger).unwrap(),
        )
        .unwrap();
        dir
    }

    fn no_lock() -> impl Fn(
        usize,
        crate::facts_apply_rpc::ApplyAttempt<'_>,
    ) -> Result<Applied, memory_core::locks::WithLockError<String>> {
        |_, _| panic!("an envelope without a planned recovery must not take the lock")
    }

    #[test]
    fn an_unplanned_unowned_envelope_is_pruned_and_a_mismatched_one_is_invisible() {
        let (_dir, identity) = identity();
        let pruned = envelope(&identity, "batch-a", "batch-a", None);
        let invisible = envelope(&identity, "batch-b", "other", None);
        let lock = no_lock();
        let active = reconcile_applied_facts_batches(&identity, &lock, &mut |_, _, _| {
            panic!("no warning expected")
        });
        assert!(!active);
        assert!(!pruned.exists());
        assert!(invisible.exists());
    }

    #[test]
    fn an_envelope_this_process_is_applying_stays_owned() {
        let (_dir, identity) = identity();
        let dir = envelope(&identity, "batch-c", "batch-c", Some(std::process::id()));
        active_applied_batches().lock().unwrap().insert(dir.clone());
        let lock = no_lock();
        assert!(reconcile_applied_facts_batches(
            &identity,
            &lock,
            &mut |_, _, _| {}
        ));
        assert!(dir.exists());
        active_applied_batches().lock().unwrap().remove(&dir);
        // Once this process stops applying it, the same envelope (no planned recovery) is pruned.
        assert!(!reconcile_applied_facts_batches(
            &identity,
            &lock,
            &mut |_, _, _| {}
        ));
        assert!(!dir.exists());
    }
}
