//! Retention of folded reflection run directories (`reflection/runs/<runId>/`, design 2.1).

use std::path::Path;

use serde_json::Value;

use crate::fs::remove_tree::remove_tree;
use crate::locks::acquire::{AcquireLockError, AcquireLockOptions, acquire_lock, release_lock};
use crate::locks::domains::run_finalization_lock_path;
use crate::locks::lock_record::{CreateLockRecordOptions, create_lock_record};
use crate::reflection::run_id_high_water::reflection_run_number;
use crate::support::random::random_uuid;
use crate::support::time::parse_rfc3339;

use super::budget::{PassBudget, RetentionStore};
use super::guard::{LazyRetentionGuard, reserved_run_ids};
use super::policy::{
    RetentionContext, RetentionKeepReason, RetentionStoreResult, StoreTally, cutoff_for_days,
    idle_since, list_names,
};

pub const RETENTION_TOMBSTONE_PREFIX: &str = ".prune-";
const FOLDED_OUTCOMES: &[&str] = &["merged", "no_changes"];
const TERMINAL_SENTINELS: &[&str] = &["abandoned.json", "quarantined.json"];

#[derive(Debug, Clone)]
pub struct FoldedRun {
    pub name: String,
    pub finished_at: i64,
}

pub enum RunClassification {
    Folded(FoldedRun),
    Keep(RetentionKeepReason),
}

pub fn classify_reflection_run(run_dir: &Path, name: &str) -> RunClassification {
    let raw = match std::fs::read_to_string(run_dir.join("final.json")) {
        Ok(raw) => raw,
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            let has_terminal = TERMINAL_SENTINELS
                .iter()
                .any(|sentinel| run_dir.join(sentinel).exists());
            return RunClassification::Keep(if has_terminal {
                RetentionKeepReason::UnfoldedTerminal
            } else {
                RetentionKeepReason::NoTerminal
            });
        }
        Err(_) => return RunClassification::Keep(RetentionKeepReason::Malformed),
    };

    let Ok(val) = serde_json::from_str::<Value>(&raw) else {
        return RunClassification::Keep(RetentionKeepReason::Malformed);
    };
    let Some(obj) = val.as_object() else {
        return RunClassification::Keep(RetentionKeepReason::Malformed);
    };
    if obj.get("runId").and_then(Value::as_str) != Some(name) {
        return RunClassification::Keep(RetentionKeepReason::Malformed);
    }
    let Some(finished_at) = obj
        .get("finishedAt")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339)
    else {
        return RunClassification::Keep(RetentionKeepReason::Malformed);
    };
    let Some(outcome) = obj.get("outcome").and_then(Value::as_str) else {
        return RunClassification::Keep(RetentionKeepReason::Malformed);
    };
    if FOLDED_OUTCOMES.contains(&outcome) {
        RunClassification::Folded(FoldedRun {
            name: name.to_owned(),
            finished_at,
        })
    } else {
        RunClassification::Keep(RetentionKeepReason::UnfoldedTerminal)
    }
}

pub fn remove_tombstone(path: &Path, context: &RetentionContext<'_>) -> bool {
    match remove_tree(path) {
        Ok(()) => true,
        Err(err) => {
            context.warn_msg(
                "memory runtime retention could not delete a tombstone",
                &format!("{}: {err}", path.display()),
            );
            false
        }
    }
}

pub fn claim_reflection_run(
    context: &RetentionContext<'_>,
    runs_dir: &Path,
    name: &str,
    cutoff_ms: i64,
) -> Result<Option<RetentionKeepReason>, String> {
    let lock_path =
        run_finalization_lock_path(&context.paths.locks, name).map_err(|e| e.to_string())?;
    let record = create_lock_record("runtime-retention", CreateLockRecordOptions::default())
        .map_err(|e| e.to_string())?;
    let acquire_opts = AcquireLockOptions {
        wait_timeout_ms: Some(0),
        retry_delay_ms: None,
        cancellation: None,
    };
    match acquire_lock(&lock_path, &record, &acquire_opts) {
        Ok(()) => {}
        Err(AcquireLockError::Contention(_)) => return Ok(Some(RetentionKeepReason::Locked)),
        Err(err) => return Err(err.to_string()),
    }

    let run_dir = runs_dir.join(name);
    let tombstone = runs_dir.join(format!(
        "{RETENTION_TOMBSTONE_PREFIX}{name}-{}",
        random_uuid()
    ));

    let claim_result: Result<(), RetentionKeepReason> = (|| {
        if reserved_run_ids(&context.paths.reflection).contains(name) {
            return Err(RetentionKeepReason::Reserved);
        }
        if !idle_since(&run_dir, cutoff_ms) {
            return Err(RetentionKeepReason::Recent);
        }
        if let Some(before) = context.before_claim_rename
            && let Err(err) = before(&run_dir, &tombstone)
        {
            context.warn_msg(
                "memory runtime retention could not claim a run directory",
                &format!("{name}: {err}"),
            );
            return Err(RetentionKeepReason::ClaimFailed);
        }
        if let Err(err) = std::fs::rename(&run_dir, &tombstone) {
            context.warn_msg(
                "memory runtime retention could not claim a run directory",
                &format!("{name}: {err}"),
            );
            return Err(RetentionKeepReason::ClaimFailed);
        }
        Ok(())
    })();

    let _ = release_lock(&lock_path, &record);
    if let Err(reason) = claim_result {
        return Ok(Some(reason));
    }

    if let Some(before_del) = context.before_tombstone_delete {
        before_del(&tombstone);
    }
    let _ = remove_tombstone(&tombstone, context);
    Ok(None)
}

pub fn prune_reflection_runs(
    context: &RetentionContext<'_>,
    lazy_guard: &mut LazyRetentionGuard<'_>,
    budget: &mut PassBudget<'_>,
) -> Result<RetentionStoreResult, String> {
    let runs_dir = context.paths.reflection.join("runs");
    let mut tally = StoreTally::default();
    let mut folded: Vec<FoldedRun> = Vec::new();
    let mut crash_tombstones = Vec::new();

    for name in list_names(&runs_dir) {
        if !budget.proceed(RetentionStore::Runs) {
            return Ok(tally.result());
        }
        let run_dir = runs_dir.join(&name);
        if name.starts_with(RETENTION_TOMBSTONE_PREFIX) {
            crash_tombstones.push(run_dir);
            continue;
        }
        match classify_reflection_run(&run_dir, &name) {
            RunClassification::Keep(reason) => tally.keep(name, reason),
            RunClassification::Folded(run) => folded.push(run),
        }
    }

    for tomb in crash_tombstones {
        if !budget.admit(RetentionStore::Runs) {
            return Ok(tally.result());
        }
        let _ = remove_tombstone(&tomb, context);
    }

    let newest = folded
        .iter()
        .max_by(|l, r| {
            l.finished_at
                .cmp(&r.finished_at)
                .then_with(|| l.name.cmp(&r.name))
        })
        .map(|r| r.name.clone());
    let cutoff = cutoff_for_days(context.now(), context.policy.retention_days);

    for run in folded {
        if budget.skip(RetentionStore::Runs, &run.name) {
            continue;
        }
        if !budget.admit(RetentionStore::Runs) {
            break;
        }
        let run_dir = runs_dir.join(&run.name);
        if newest.as_deref() == Some(run.name.as_str()) {
            tally.keep(run.name.clone(), RetentionKeepReason::Anchor);
        } else if run.finished_at > cutoff || !idle_since(&run_dir, cutoff) {
            tally.keep(run.name.clone(), RetentionKeepReason::Recent);
        } else {
            let guard = lazy_guard.get()?;
            match guard {
                None => tally.keep(run.name.clone(), RetentionKeepReason::SchedulerBusy),
                Some(g) if g.reserved.contains(&run.name) => {
                    tally.keep(run.name.clone(), RetentionKeepReason::Reserved);
                }
                Some(g) if reflection_run_number(&run.name) == g.high_water => {
                    tally.keep(run.name.clone(), RetentionKeepReason::Anchor);
                }
                Some(_) => match claim_reflection_run(context, &runs_dir, &run.name, cutoff)? {
                    None => tally.removed.push(run.name.clone()),
                    Some(reason) => tally.keep(run.name.clone(), reason),
                },
            }
        }
        budget.done(RetentionStore::Runs, &run.name);
    }

    Ok(tally.result())
}
