//! Plan persistence and transaction coordination.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::constants::{ULW_LOOP_DIR, ULW_LOOP_GOALS, ULW_LOOP_LEDGER};
use crate::domain_types::{UlwLoopLedgerEntry, UlwLoopPlan};
use crate::goal_status::aggregate_codex_objective_for_scope;
use crate::ledger::{read_ledger as reconcile_ledger, read_ledger_at};
use crate::paths::{
    repo_relative, ulw_loop_dir, ulw_loop_goals_path, ulw_loop_relative_dir,
    ulw_loop_state_lock_path, UlwLoopScope,
};
use crate::plan_commit::{commit, materialize_sync, CommitMutation};
use crate::plan_log::reconcile_plan;
use crate::plan_missing_recovery::plan_missing_error;
use crate::runtime::{iso, UlwLoopError};
use crate::state_lock::{with_state_lock, StateLockOptions};

pub use crate::plan_log::plan_exists;

const LEGACY_OBJECTIVE_PREFIX: &str = "Complete all ulw-loop stories in .omo/ulw-loop/goals.json: ";
const LEGACY_OBJECTIVE: &str = "Complete all ulw-loop stories listed in .omo/ulw-loop/goals.json. Use .omo/ulw-loop/ledger.jsonl as the durable audit trail.";

pub fn read_ledger(repo_root: impl AsRef<Path>, scope: Option<&UlwLoopScope>) -> Vec<UlwLoopLedgerEntry> {
    reconcile_ledger(repo_root, scope)
}

pub fn with_ulw_loop_mutation_lock<T>(
    repo_root: impl AsRef<Path>,
    scope: Option<&UlwLoopScope>,
    options: Option<&StateLockOptions>,
    f: impl FnOnce() -> Result<T, UlwLoopError>,
) -> Result<T, UlwLoopError> {
    let lock_path = ulw_loop_state_lock_path(&repo_root, scope);
    with_state_lock(&lock_path, options, |_token| f())
}

pub fn read_ulw_loop_plan_sync(
    repo_root: impl AsRef<Path>,
    scope: Option<&UlwLoopScope>,
) -> Result<UlwLoopPlan, UlwLoopError> {
    let path = ulw_loop_goals_path(&repo_root, scope);
    let dir = ulw_loop_dir(&repo_root, scope);
    let mut parsed = match reconcile_plan(&dir) {
        Some(p) => p,
        None => {
            let rel = repo_relative(&path.to_string_lossy(), &repo_root.as_ref().to_string_lossy());
            // Pin `plan-io.ts:73-81`: a goals.json that exists but is not a version-1 plan with a
            // `goals` array is an invalid plan, never a missing one.
            if fs::read_to_string(&path).is_ok() {
                return Err(UlwLoopError::new(
                    format!("Invalid ulw-loop plan at {rel}."),
                    "ULW_LOOP_PLAN_INVALID",
                ));
            }
            let session_ids = list_ulw_loop_session_ids(&repo_root);
            return Err(plan_missing_error(&rel, &session_ids, None));
        }
    };

    if parsed.version != 1 {
        let rel = repo_relative(&path.to_string_lossy(), &repo_root.as_ref().to_string_lossy());
        return Err(UlwLoopError::new(
            format!("Invalid ulw-loop plan at {rel}."),
            "ULW_LOOP_PLAN_INVALID",
        ));
    }

    let is_aggregate = parsed.codex_goal_mode.as_deref().unwrap_or("per_story") == "aggregate";
    if is_aggregate {
        if let Some(previous_objective) = parsed.codex_objective.clone() {
            if previous_objective == LEGACY_OBJECTIVE || previous_objective.starts_with(LEGACY_OBJECTIVE_PREFIX) {
                let now = iso();
                let new_obj = aggregate_codex_objective_for_scope(scope);
                let mut aliases = parsed.codex_objective_aliases.clone().unwrap_or_default();
                if !aliases.contains(&previous_objective) {
                    aliases.push(previous_objective.clone());
                }
                parsed.codex_objective = Some(new_obj.clone());
                parsed.codex_objective_aliases = Some(aliases);
                parsed.updated_at = now.clone();
                let mut entries = vec![UlwLoopLedgerEntry {
                    at: now,
                    kind: "aggregate_objective_migrated".to_string(),
                    message: Some(
                        "Migrated legacy enumerated aggregate Codex objective to the stable pointer objective."
                            .to_string(),
                    ),
                    before: Some(serde_json::json!({ "codexObjective": previous_objective })),
                    after: Some(serde_json::json!({ "codexObjective": new_obj })),
                    ..Default::default()
                }];
                commit(
                    &repo_root,
                    scope,
                    CommitMutation {
                        plan: &mut parsed,
                        entries: &mut entries,
                    },
                )?;
            }
        }
    }

    Ok(parsed)
}

pub fn read_ulw_loop_plan(
    repo_root: impl AsRef<Path>,
    scope: Option<&UlwLoopScope>,
) -> Result<UlwLoopPlan, UlwLoopError> {
    let dir = ulw_loop_dir(&repo_root, scope);
    let _ = materialize_sync(&dir);
    read_ulw_loop_plan_sync(repo_root, scope)
}

pub fn list_ulw_loop_session_ids(repo_root: impl AsRef<Path>) -> Vec<String> {
    let dir = ulw_loop_dir(repo_root, None);
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut session_ids = Vec::new();
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name != "revisions" && name != "tmp" && !name.starts_with('.') {
                session_ids.push(name);
            }
        }
    }
    session_ids.sort();
    session_ids
}

pub fn write_plan(
    repo_root: impl AsRef<Path>,
    mut plan: UlwLoopPlan,
    scope: Option<&UlwLoopScope>,
) -> Result<(), UlwLoopError> {
    if plan.revision.is_none() {
        plan.revision = reconcile_plan(ulw_loop_dir(&repo_root, scope)).and_then(|current| current.revision);
    }
    let mut entries = Vec::new();
    commit(
        repo_root,
        scope,
        CommitMutation {
            plan: &mut plan,
            entries: &mut entries,
        },
    )
}

pub fn append_ledger(
    repo_root: impl AsRef<Path>,
    entry: UlwLoopLedgerEntry,
    scope: Option<&UlwLoopScope>,
) -> Result<(), UlwLoopError> {
    append_ledger_entries(repo_root, vec![entry], scope)
}

pub fn append_ledger_entries(
    repo_root: impl AsRef<Path>,
    mut entries: Vec<UlwLoopLedgerEntry>,
    scope: Option<&UlwLoopScope>,
) -> Result<(), UlwLoopError> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut plan = read_ulw_loop_plan(&repo_root, scope)?;
    commit(
        repo_root,
        scope,
        CommitMutation {
            plan: &mut plan,
            entries: &mut entries,
        },
    )
}

fn is_steering_kind(kind: &str) -> bool {
    matches!(
        kind,
        "steering_accepted" | "steering_rejected" | "criteria_revised" | "batch_updated"
    )
}

pub fn read_steering_ledger_entries(
    repo_root: impl AsRef<Path>,
    scope: Option<&UlwLoopScope>,
) -> Vec<UlwLoopLedgerEntry> {
    read_ledger(repo_root, scope)
        .into_iter()
        .filter(|entry| is_steering_kind(&entry.kind))
        .collect()
}

pub fn find_accepted_steering_ledger_entry(
    repo_root: impl AsRef<Path>,
    key: &str,
    scope: Option<&UlwLoopScope>,
) -> Option<UlwLoopLedgerEntry> {
    find_accepted_steering_entry(&read_ledger(repo_root, scope), key).cloned()
}

/// [`find_accepted_steering_ledger_entry`] over an already-read ledger, so a batch reads it once.
pub fn find_accepted_steering_entry<'a>(
    entries: &'a [UlwLoopLedgerEntry],
    key: &str,
) -> Option<&'a UlwLoopLedgerEntry> {
    entries.iter().find(|entry| {
        is_steering_kind(&entry.kind)
            && entry.steering.as_ref().is_some_and(|s| s.invariant.accepted)
            && (entry.idempotency_key.as_deref() == Some(key)
                || entry
                    .steering
                    .as_ref()
                    .and_then(|s| s.idempotency_key.as_deref())
                    == Some(key)
                || entry
                    .steering
                    .as_ref()
                    .and_then(|s| s.prompt_signature.as_deref())
                    == Some(key))
    })
}
