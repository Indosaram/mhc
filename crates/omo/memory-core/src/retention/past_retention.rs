//! Read-only counts of items past their retention window (for `/doctor` and `--fix --dry-run`) (#12532, #12932).

use std::collections::BTreeSet;

use crate::identity::layout::MemoryIdentityPaths;
use crate::reflection::run_id_high_water::{
    RunIdHighWaterPaths, read_reflection_run_id_mark, reflection_run_number,
    scan_reflection_run_high_water,
};

use super::completions::{COMPLETIONS_KEEP_NEWEST, read_completion};
use super::journals::{JournalClass, JournalVerdict, classify_journal_entry};
use super::policy::{
    RetentionContext, RetentionKeepReason, RuntimeRetentionPolicy, cutoff_for_days, idle_since,
    list_names,
};
use super::reflection_health_signal::read_unreflected_retention_gate;
use super::reflection_runs::{
    RETENTION_TOMBSTONE_PREFIX, RunClassification, classify_reflection_run,
};
use super::sidecars::{is_valid_encoded_session, kibitzer_sidecars_root};
use super::size::measure_tree_bytes;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimePastRetention {
    pub runs: usize,
    pub journals: usize,
    pub sidecars: usize,
    pub unclassified_journals: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimePastRetentionOtherStores {
    pub completions: usize,
    pub ledgers: usize,
    pub legacy: usize,
}

fn past_retention_run_names(
    paths: &MemoryIdentityPaths,
    policy: &RuntimeRetentionPolicy,
    now_ms: i64,
) -> Vec<String> {
    let cutoff = cutoff_for_days(now_ms, policy.retention_days);
    let runs_dir = paths.reflection.join("runs");
    let high_water = scan_reflection_run_high_water(&RunIdHighWaterPaths {
        reflection: paths.reflection.clone(),
        reflection_sessions: paths.reflection_sessions.clone(),
        worktrees: paths.worktrees.clone(),
    });
    let mut folded = Vec::new();
    for name in list_names(&runs_dir) {
        if name.starts_with(RETENTION_TOMBSTONE_PREFIX) {
            continue;
        }
        if let RunClassification::Folded(run) =
            classify_reflection_run(&runs_dir.join(&name), &name)
        {
            folded.push(run);
        }
    }
    let newest = folded
        .iter()
        .max_by(|l, r| {
            l.finished_at
                .cmp(&r.finished_at)
                .then_with(|| l.name.cmp(&r.name))
        })
        .map(|r| r.name.clone());

    let mut names = Vec::new();
    for run in folded {
        if newest.as_deref() == Some(run.name.as_str())
            || reflection_run_number(&run.name) == high_water
            || run.finished_at > cutoff
        {
            continue;
        }
        if idle_since(&runs_dir.join(&run.name), cutoff) {
            names.push(run.name);
        }
    }
    names
}

pub fn count_runtime_past_retention(
    paths: &MemoryIdentityPaths,
    policy: &RuntimeRetentionPolicy,
    now_ms: &dyn Fn() -> i64,
    is_live_session: Option<&dyn Fn(&str) -> bool>,
    reflection_enabled: bool,
    aborted: Option<&dyn Fn() -> bool>,
) -> RuntimePastRetention {
    let now = now_ms();
    let cutoff = cutoff_for_days(now, policy.retention_days);
    let runs = past_retention_run_names(paths, policy, now).len();

    let mut journals = 0usize;
    let mut unclassified_journals = 0usize;
    let context = RetentionContext {
        paths,
        policy: policy.clone(),
        now_ms,
        is_live_session,
        reflection_enabled,
        aborted,
        warn: None,
        before_claim_rename: None,
        before_journal_claim: None,
        before_tombstone_delete: None,
    };
    let mut admit = || !aborted.is_some_and(|f| f());
    let gate = read_unreflected_retention_gate(&paths.reflection, reflection_enabled, &mut admit);
    for session_id in list_names(&paths.transcripts) {
        if is_live_session.is_some_and(|live| live(&session_id)) {
            continue;
        }
        match classify_journal_entry(
            &context,
            &paths.transcripts.join(&session_id),
            gate.as_ref(),
        ) {
            None => {}
            Some(JournalVerdict::Prune(_)) => journals += 1,
            Some(JournalVerdict::Keep(RetentionKeepReason::Unclassified)) => {
                unclassified_journals += 1;
            }
            Some(JournalVerdict::Keep(_)) => {}
        }
    }

    let mut sidecars = 0usize;
    let root = kibitzer_sidecars_root(&paths.recall);
    for name in list_names(&root) {
        if !is_valid_encoded_session(&name) {
            continue;
        }
        let p = root.join(&name);
        if p.is_dir() && idle_since(&p, cutoff) {
            sidecars += 1;
        }
    }

    RuntimePastRetention {
        runs,
        journals,
        sidecars,
        unclassified_journals,
    }
}

pub fn measure_unreflected_journal_bytes(
    paths: &MemoryIdentityPaths,
    policy: &RuntimeRetentionPolicy,
    aborted: Option<&dyn Fn() -> bool>,
) -> Result<u64, String> {
    let far_future = || i64::MAX / 2;
    let context = RetentionContext {
        paths,
        policy: RuntimeRetentionPolicy {
            retention_days: 0,
            abandoned_journal_days: 0,
            discard_unreflected_journals: true,
            ..policy.clone()
        },
        now_ms: &far_future,
        is_live_session: None,
        reflection_enabled: true,
        aborted,
        warn: None,
        before_claim_rename: None,
        before_journal_claim: None,
        before_tombstone_delete: None,
    };
    let open_gate = super::reflection_health_signal::UnreflectedRetentionGate {
        open: true,
        newest: None,
    };
    let mut bytes = 0u64;
    for session_id in list_names(&paths.transcripts) {
        let dir = paths.transcripts.join(&session_id);
        match classify_journal_entry(&context, &dir, Some(&open_gate)) {
            None => {}
            Some(JournalVerdict::Keep(RetentionKeepReason::StateBehindTranscript))
            | Some(JournalVerdict::Prune(JournalClass::Never)) => {
                bytes = bytes.saturating_add(
                    measure_tree_bytes(&dir, aborted).map_err(|_| "Aborted".to_owned())?,
                );
            }
            Some(JournalVerdict::Prune(JournalClass::Partly)) => {
                bytes = bytes.saturating_add(
                    measure_tree_bytes(&dir.join("transcript.jsonl"), aborted)
                        .map_err(|_| "Aborted".to_owned())?,
                );
            }
            _ => {}
        }
    }
    Ok(bytes)
}

/// Counts completions (including those whose run dir the same pass removes!), recall ledgers, and
/// legacy directories past retention for `--fix --dry-run` (#12932).
pub fn count_runtime_past_retention_other_stores(
    paths: &MemoryIdentityPaths,
    policy: &RuntimeRetentionPolicy,
    now_ms: i64,
    aborted: Option<&dyn Fn() -> bool>,
) -> Result<RuntimePastRetentionOtherStores, String> {
    let high_water = scan_reflection_run_high_water(&RunIdHighWaterPaths {
        reflection: paths.reflection.clone(),
        reflection_sessions: paths.reflection_sessions.clone(),
        worktrees: paths.worktrees.clone(),
    });
    // The pass prunes runs before completions, so a run the pass removes no longer protects its completion.
    let removed_runs: BTreeSet<String> = past_retention_run_names(paths, policy, now_ms)
        .into_iter()
        .collect();
    let mark = read_reflection_run_id_mark(&paths.reflection)
        .unwrap_or(0)
        .max(high_water);
    let completions_cutoff = cutoff_for_days(now_ms, policy.completions_days);
    let retention_cutoff = cutoff_for_days(now_ms, policy.retention_days);

    let comp_dir = paths.reflection.join("completions");
    let mut records = Vec::new();
    for name in list_names(&comp_dir) {
        if aborted.is_some_and(|f| f()) {
            return Err("Aborted".to_owned());
        }
        let Some(run_id) = name.strip_suffix(".json") else {
            continue;
        };
        if let Some(record) = read_completion(&comp_dir.join(&name), run_id) {
            records.push(record);
        }
    }
    let mut newest_first = records.clone();
    newest_first.sort_by(|l, r| {
        r.finished_at
            .cmp(&l.finished_at)
            .then_with(|| r.run_id.cmp(&l.run_id))
    });
    let newest: BTreeSet<String> = newest_first
        .into_iter()
        .take(COMPLETIONS_KEEP_NEWEST)
        .map(|r| r.run_id)
        .collect();

    let mut completions = 0usize;
    for record in records {
        let number = reflection_run_number(&record.run_id);
        if newest.contains(&record.run_id)
            || !record.consumed
            || record.finished_at > completions_cutoff
        {
            continue;
        }
        if !removed_runs.contains(&record.run_id)
            && paths.reflection.join("runs").join(&record.run_id).exists()
        {
            continue;
        }
        if number == 0 || number > mark || number == high_water {
            continue;
        }
        completions += 1;
    }

    let mut ledgers = 0usize;
    for name in list_names(&paths.recall_ledger) {
        if aborted.is_some_and(|f| f()) {
            return Err("Aborted".to_owned());
        }
        if !name.ends_with(".json") {
            continue;
        }
        let p = paths.recall_ledger.join(&name);
        if p.is_file()
            && let Ok(meta) = std::fs::symlink_metadata(&p)
            && let Ok(mtime) = meta.modified()
            && let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH)
            && i64::try_from(dur.as_millis()).is_ok_and(|ms| ms <= completions_cutoff)
        {
            ledgers += 1;
        }
    }

    let mut legacy = 0usize;
    for name in list_names(&paths.reflection_sessions) {
        if aborted.is_some_and(|f| f()) {
            return Err("Aborted".to_owned());
        }
        if reflection_run_number(&name) != high_water
            && idle_since(&paths.reflection_sessions.join(&name), retention_cutoff)
        {
            legacy += 1;
        }
    }
    let recall_runs = paths.recall.join("runs");
    if recall_runs.exists() && idle_since(&recall_runs, retention_cutoff) {
        legacy += 1;
    }

    Ok(RuntimePastRetentionOtherStores {
        completions,
        ledgers,
        legacy,
    })
}
