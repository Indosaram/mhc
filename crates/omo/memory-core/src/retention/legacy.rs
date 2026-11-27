//! Legacy runtime stores retention (`recall/runs/` and `reflection-sessions/<runId>/`, design 2.6).

use crate::fs::remove_tree::remove_tree;
use crate::reflection::run_id_high_water::reflection_run_number;

use super::budget::{PassBudget, RetentionStore};
use super::guard::LazyRetentionGuard;
use super::policy::{
    RetentionContext, RetentionKeepReason, RetentionStoreResult, StoreTally, cutoff_for_days,
    idle_since, list_names,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyRecallRunsOutcome {
    Absent,
    Kept,
    Removed,
}

impl LegacyRecallRunsOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Kept => "kept",
            Self::Removed => "removed",
        }
    }
}

pub fn prune_legacy_recall_runs(
    context: &RetentionContext<'_>,
    budget: &mut PassBudget<'_>,
) -> std::io::Result<LegacyRecallRunsOutcome> {
    let dir = context.paths.recall.join("runs");
    if !dir.exists() {
        return Ok(LegacyRecallRunsOutcome::Absent);
    }
    if budget.skip(RetentionStore::RecallRuns, "runs") || !budget.admit(RetentionStore::RecallRuns)
    {
        return Ok(LegacyRecallRunsOutcome::Kept);
    }
    let cutoff = cutoff_for_days(context.now(), context.policy.retention_days);
    if !idle_since(&dir, cutoff) {
        return Ok(LegacyRecallRunsOutcome::Kept);
    }
    remove_tree(&dir)?;
    budget.done(RetentionStore::RecallRuns, "runs");
    Ok(LegacyRecallRunsOutcome::Removed)
}

pub fn prune_legacy_reflection_sessions(
    context: &RetentionContext<'_>,
    lazy_guard: &mut LazyRetentionGuard<'_>,
    budget: &mut PassBudget<'_>,
) -> Result<RetentionStoreResult, String> {
    let mut tally = StoreTally::default();
    let cutoff = cutoff_for_days(context.now(), context.policy.retention_days);
    for name in list_names(&context.paths.reflection_sessions) {
        if budget.skip(RetentionStore::ReflectionSessions, &name) {
            continue;
        }
        if !budget.admit(RetentionStore::ReflectionSessions) {
            break;
        }
        let path = context.paths.reflection_sessions.join(&name);
        if !idle_since(&path, cutoff) {
            tally.keep(name.clone(), RetentionKeepReason::Recent);
        } else {
            let guard = lazy_guard.get()?;
            match guard {
                None => tally.keep(name.clone(), RetentionKeepReason::SchedulerBusy),
                Some(g) if reflection_run_number(&name) == g.high_water => {
                    tally.keep(name.clone(), RetentionKeepReason::Anchor);
                }
                Some(_) => match remove_tree(&path) {
                    Ok(()) => tally.removed.push(name.clone()),
                    Err(err) => {
                        context.warn_msg(
                            "memory runtime retention could not remove a legacy reflection session",
                            &format!("{}: {err}", path.display()),
                        );
                        tally.keep(name.clone(), RetentionKeepReason::ChangedDuringRemoval);
                    }
                },
            }
        }
        budget.done(RetentionStore::ReflectionSessions, &name);
    }
    Ok(tally.result())
}
