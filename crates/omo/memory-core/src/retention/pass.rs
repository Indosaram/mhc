//! Per-identity runtime retention pass (design 2, section 6, #12532, #12538, #10144, #12932).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::journal::lock::JournalLock;

use super::budget::{PassBudget, RetentionCursor, RetentionStore};
use super::cap::{
    RuntimeCapCursor, RuntimeSizeCapOptions, RuntimeSizeCapReport, run_runtime_size_cap,
};
use super::completions::prune_completions;
use super::guard::LazyRetentionGuard;
use super::journals::prune_transcript_journals;
use super::legacy::{
    LegacyRecallRunsOutcome, prune_legacy_recall_runs, prune_legacy_reflection_sessions,
};
use super::log::{RetentionLogInput, append_retention_log};
use super::policy::{DAY_MS, RetentionContext, RetentionStoreResult};
use super::recall_ledgers::prune_recall_ledgers;
use super::reflection_health_signal::{UnreflectedRetentionGate, read_unreflected_retention_gate};
use super::reflection_runs::prune_reflection_runs;
use super::sidecars::{PruneKibitzerSidecarsOptions, prune_kibitzer_sidecars};
use super::stamp::{
    RetentionResumePoints, clear_retention_cursor, read_retention_resume_points,
    write_retention_resume_points,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetentionStopCursor {
    Store(RetentionCursor),
    Cap(RuntimeCapCursor),
}

impl RetentionStopCursor {
    pub fn store_name(&self) -> &str {
        match self {
            Self::Store(c) => &c.store,
            Self::Cap(c) => &c.store,
        }
    }
}

pub struct RuntimeRetentionOptions<'a> {
    pub context: RetentionContext<'a>,
    pub owned_sidecars: &'a BTreeSet<String>,
    pub max_bytes: Option<u64>,
    pub journal_floor_ms: Option<i64>,
    pub custom_journal_lock: Option<&'a dyn JournalLock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRetentionReport {
    pub high_water: Option<u64>,
    pub skipped: Vec<String>,
    pub runs: RetentionStoreResult,
    pub completions: RetentionStoreResult,
    pub reflection_sessions: RetentionStoreResult,
    pub journals: RetentionStoreResult,
    pub sidecars: RetentionStoreResult,
    pub ledgers: RetentionStoreResult,
    pub recall_runs: LegacyRecallRunsOutcome,
    pub unreflected_gate: Option<UnreflectedRetentionGate>,
    pub cursor: Option<RetentionStopCursor>,
    pub cap: Option<RuntimeSizeCapReport>,
}

pub fn run_runtime_retention(
    options: &RuntimeRetentionOptions<'_>,
) -> Result<RuntimeRetentionReport, String> {
    if options.context.is_aborted() {
        return Err("Retention aborted".to_owned());
    }
    let context = &options.context;
    let stored = read_retention_resume_points(&context.paths.runtime);
    let mut budget = PassBudget::new(
        context.now_ms,
        context.policy.pass_budget_ms,
        context.aborted,
        stored.cursor.clone(),
    );
    let mut guard = LazyRetentionGuard::new(context.paths);

    let runs = prune_reflection_runs(context, &mut guard, &mut budget)?;
    let completions = prune_completions(context, &mut guard, &mut budget)?;
    let reflection_sessions = prune_legacy_reflection_sessions(context, &mut guard, &mut budget)?;

    let unreflected_gate = if context.policy.discard_unreflected_journals {
        let mut admit = || budget.proceed(RetentionStore::Journals);
        read_unreflected_retention_gate(
            &context.paths.reflection,
            context.reflection_enabled,
            &mut admit,
        )
    } else {
        None
    };

    let journals = prune_transcript_journals(
        context,
        &mut guard,
        unreflected_gate.as_ref(),
        &mut budget,
        options.custom_journal_lock,
    );

    let sidecar_result = {
        let budget_cell = std::cell::RefCell::new(&mut budget);
        let mut admit_sidecars = || budget_cell.borrow_mut().admit(RetentionStore::Sidecars);
        let mut done_sidecars = |name: &str| {
            budget_cell
                .borrow_mut()
                .done(RetentionStore::Sidecars, name)
        };
        let skip_sidecars = |name: &str| budget_cell.borrow().skip(RetentionStore::Sidecars, name);
        prune_kibitzer_sidecars(PruneKibitzerSidecarsOptions {
            recall_dir: &context.paths.recall,
            locks_dir: &context.paths.locks,
            now_ms: context.now_ms,
            max_age_ms: Some(
                i64::try_from(context.policy.retention_days)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(DAY_MS),
            ),
            owned: options.owned_sidecars,
            warn: context.warn,
            before_claim_rename: context.before_claim_rename,
            before_tombstone_delete: context.before_tombstone_delete,
            skip: Some(&skip_sidecars),
            admit: Some(&mut admit_sidecars),
            done: Some(&mut done_sidecars),
        })
    };

    let ledgers = prune_recall_ledgers(context, &mut budget).map_err(|e| e.to_string())?;
    let recall_runs = prune_legacy_recall_runs(context, &mut budget).map_err(|e| e.to_string())?;

    let mut cap_resume = options.max_bytes.and(stored.cap.clone());
    let mut cap: Option<RuntimeSizeCapReport> = None;
    if let Some(max_bytes) = options.max_bytes
        && budget.stopped.is_none()
    {
        let cap_report = run_runtime_size_cap(&RuntimeSizeCapOptions {
            context,
            max_bytes,
            owned_sidecars: options.owned_sidecars,
            custom_journal_lock: options.custom_journal_lock,
            journal_floor_ms: options.journal_floor_ms,
            budget_ms: Some(budget.remaining_ms()),
            clock_ms: Some(context.now_ms),
            measure_aborted: context.aborted,
            resume: cap_resume.clone(),
        })?;
        cap_resume = cap_report.cursor.clone();
        cap = Some(cap_report);
    }

    log_removals(
        options,
        &runs,
        &completions,
        &reflection_sessions,
        &journals,
        &sidecar_result.pruned,
        &ledgers,
        recall_runs,
        cap.as_ref(),
    );

    let cap_stalled = cap
        .as_ref()
        .is_some_and(|c| c.skipped.iter().any(|s| s == "measure-over-budget"));
    if budget.stopped.is_some() || cap_resume.is_some() {
        write_retention_resume_points(
            &context.paths.runtime,
            &RetentionResumePoints {
                cursor: budget.stopped.clone(),
                cap: cap_resume.clone(),
                cap_stalled: cap_stalled.then_some(true),
            },
        )
        .map_err(|e| e.to_string())?;
        if budget.aborted {
            return Err("Retention aborted".to_owned());
        }
    } else if stored.cursor.is_some() || stored.cap.is_some() {
        clear_retention_cursor(&context.paths.runtime).map_err(|e| e.to_string())?;
    }

    let cursor = budget.stopped.map(RetentionStopCursor::Store).or_else(|| {
        cap.as_ref()
            .and_then(|c| c.cursor.clone())
            .map(RetentionStopCursor::Cap)
    });

    let mut sidecar_pruned = sidecar_result.pruned;
    sidecar_pruned.sort();
    let sidecars = RetentionStoreResult {
        removed: sidecar_pruned,
        kept: sidecar_result.kept_reasons,
    };

    Ok(RuntimeRetentionReport {
        high_water: guard.value.map(|g| g.high_water),
        skipped: if guard.busy {
            vec!["reflection-scheduler-busy".to_owned()]
        } else {
            Vec::new()
        },
        runs,
        completions,
        reflection_sessions,
        journals,
        sidecars,
        ledgers,
        recall_runs,
        unreflected_gate,
        cursor,
        cap,
    })
}

#[allow(clippy::too_many_arguments)]
fn log_removals(
    options: &RuntimeRetentionOptions<'_>,
    runs: &RetentionStoreResult,
    completions: &RetentionStoreResult,
    reflection_sessions: &RetentionStoreResult,
    journals: &RetentionStoreResult,
    sidecars: &[String],
    ledgers: &RetentionStoreResult,
    recall_runs: LegacyRecallRunsOutcome,
    cap: Option<&RuntimeSizeCapReport>,
) {
    let mut removed = BTreeMap::new();
    removed.insert("runs".to_owned(), runs.removed.clone());
    removed.insert("completions".to_owned(), completions.removed.clone());
    removed.insert(
        "reflectionSessions".to_owned(),
        reflection_sessions.removed.clone(),
    );
    removed.insert("journals".to_owned(), journals.removed.clone());
    let mut sorted_sidecars = sidecars.to_vec();
    sorted_sidecars.sort();
    removed.insert("sidecars".to_owned(), sorted_sidecars);
    removed.insert("ledgers".to_owned(), ledgers.removed.clone());
    if recall_runs == LegacyRecallRunsOutcome::Removed {
        removed.insert("legacy".to_owned(), vec!["recall/runs".to_owned()]);
    }
    let mut cap_evicted_bytes = 0u64;
    if let Some(cap) = cap {
        for eviction in &cap.evicted {
            removed
                .entry(format!("cap:{}", eviction.tier.as_str()))
                .or_default()
                .push(eviction.name.clone());
            cap_evicted_bytes = cap_evicted_bytes.saturating_add(eviction.bytes);
        }
    }
    if let Err(err) = append_retention_log(
        &options.context.paths.runtime,
        &RetentionLogInput {
            at: options.context.now(),
            removed,
            cap_evicted_bytes: (cap_evicted_bytes > 0).then_some(cap_evicted_bytes),
        },
        None,
        None,
    ) {
        options.context.warn_msg(
            "memory runtime retention could not write its removal log",
            &err.to_string(),
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRetentionSummary {
    pub removed: usize,
    pub runs: usize,
    pub completions: usize,
    pub reflection_sessions: usize,
    pub journals: usize,
    pub sidecars: usize,
    pub ledgers: usize,
    pub cap_evicted: usize,
    pub fields: Map<String, Value>,
}

pub fn summarize_runtime_retention(report: &RuntimeRetentionReport) -> RuntimeRetentionSummary {
    let cap_evicted = report.cap.as_ref().map_or(0, |c| c.evicted.len());
    let removed = report.runs.removed.len()
        + report.completions.removed.len()
        + report.reflection_sessions.removed.len()
        + report.journals.removed.len()
        + report.sidecars.removed.len()
        + report.ledgers.removed.len()
        + usize::from(report.recall_runs == LegacyRecallRunsOutcome::Removed)
        + cap_evicted;

    let mut fields = Map::new();
    fields.insert("removed".to_owned(), (removed as u64).into());
    fields.insert("runs".to_owned(), (report.runs.removed.len() as u64).into());
    fields.insert(
        "completions".to_owned(),
        (report.completions.removed.len() as u64).into(),
    );
    fields.insert(
        "reflectionSessions".to_owned(),
        (report.reflection_sessions.removed.len() as u64).into(),
    );
    fields.insert(
        "journals".to_owned(),
        (report.journals.removed.len() as u64).into(),
    );
    fields.insert(
        "sidecars".to_owned(),
        (report.sidecars.removed.len() as u64).into(),
    );
    fields.insert(
        "ledgers".to_owned(),
        (report.ledgers.removed.len() as u64).into(),
    );
    fields.insert(
        "recallRuns".to_owned(),
        Value::String(report.recall_runs.as_str().to_owned()),
    );
    fields.insert(
        "skipped".to_owned(),
        Value::Array(report.skipped.iter().cloned().map(Value::String).collect()),
    );
    if let Some(ref cursor) = report.cursor {
        fields.insert(
            "interruptedAt".to_owned(),
            Value::String(cursor.store_name().to_owned()),
        );
    }
    if let Some(ref cap) = report.cap {
        let evicted_bytes: u64 = cap.evicted.iter().map(|e| e.bytes).sum();
        let high_water_kept: Vec<Value> = cap
            .kept
            .iter()
            .filter(|(_, r)| **r == super::cap::RuntimeCapKeepReason::HighWater)
            .map(|(n, _)| Value::String(n.clone()))
            .collect();
        fields.insert("capBytes".to_owned(), cap.max_bytes.into());
        fields.insert("bytesBefore".to_owned(), cap.before_bytes.into());
        fields.insert("bytesAfter".to_owned(), cap.after_bytes.into());
        fields.insert("evicted".to_owned(), (cap.evicted.len() as u64).into());
        fields.insert("evictedBytes".to_owned(), evicted_bytes.into());
        fields.insert("overCapBytes".to_owned(), cap.exceeded_bytes.into());
        fields.insert("capKeptHighWater".to_owned(), Value::Array(high_water_kept));
    }

    RuntimeRetentionSummary {
        removed,
        runs: report.runs.removed.len(),
        completions: report.completions.removed.len(),
        reflection_sessions: report.reflection_sessions.removed.len(),
        journals: report.journals.removed.len(),
        sidecars: report.sidecars.removed.len(),
        ledgers: report.ledgers.removed.len(),
        cap_evicted,
        fields,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::Path;

    use crate::identity::layout::build_identity_paths;
    use crate::retention::cap::RuntimeCapTier;
    use crate::retention::past_retention::count_runtime_past_retention_other_stores;
    use crate::retention::policy::{RetentionKeepReason, RuntimeRetentionPolicy};
    use crate::retention::stamp::{retention_resume_pending, write_retention_resume_points};
    use crate::support::time::format_rfc3339_millis;

    use super::*;

    fn set_mtime(path: &Path, ms: i64) {
        let secs = ms / 1000;
        let nanos = ((ms % 1000) * 1_000_000) as u32;
        let ts = std::time::UNIX_EPOCH + std::time::Duration::new(secs as u64, nanos);
        let file = std::fs::OpenOptions::new().write(true).open(path);
        if let Ok(f) = file {
            let _ = f.set_modified(ts);
        } else {
            filetime_set(path, secs);
        }
    }

    #[cfg(unix)]
    fn filetime_set(path: &Path, secs: i64) {
        // Touch via directory/file metadata when open(write) fails on directory using touch
        let stamp = format_rfc3339_millis(secs * 1000);
        let _ = std::process::Command::new("touch")
            .args(["-d", &stamp, &path.to_string_lossy()])
            .status();
    }

    #[cfg(not(unix))]
    fn filetime_set(_path: &Path, _secs: i64) {}

    #[test]
    fn tolerates_unreadable_journal_evicts_oldest_first_and_persists_cap_resume_points() {
        let dir = tempfile::tempdir().unwrap();
        let paths = build_identity_paths(dir.path(), "agent");
        std::fs::create_dir_all(&paths.transcripts).unwrap();
        std::fs::create_dir_all(paths.reflection.join("runs")).unwrap();

        // Corrupt/unreadable journal
        let bad_journal = paths.transcripts.join("corrupt-session");
        std::fs::create_dir_all(&bad_journal).unwrap();
        std::fs::write(bad_journal.join("state.json"), "{broken json").unwrap();

        // Two old folded runs + newest anchor + high-water
        let now = 1_750_000_000_000i64;
        let old_ts = format_rfc3339_millis(now - 10 * DAY_MS);
        let newer_ts = format_rfc3339_millis(now - 9 * DAY_MS);
        let newest_ts = format_rfc3339_millis(now - DAY_MS);
        for (name, ts, payload_size) in [
            ("reflection-run-1", &old_ts, 4096usize),
            ("reflection-run-2", &newer_ts, 4096usize),
            ("reflection-run-3", &newest_ts, 64usize),
        ] {
            let rdir = paths.reflection.join("runs").join(name);
            std::fs::create_dir_all(&rdir).unwrap();
            let fin = rdir.join("final.json");
            std::fs::write(
                &fin,
                format!(r#"{{"runId":"{name}","finishedAt":"{ts}","outcome":"merged"}}"#),
            )
            .unwrap();
            let blob = rdir.join("payload.bin");
            std::fs::write(&blob, vec![b'x'; payload_size]).unwrap();
            let mtime = crate::support::time::parse_rfc3339(ts).unwrap();
            set_mtime(&fin, mtime);
            set_mtime(&blob, mtime);
            set_mtime(&rdir, mtime);
        }

        // Write completions for run-1 and run-2 (older than completions_days) plus
        // COMPLETIONS_KEEP_NEWEST newer filler completions so run-1 and run-2 fall outside
        // the keep-newest floor and dry-run preview counts them too.
        let comp_dir = paths.reflection.join("completions");
        std::fs::create_dir_all(&comp_dir).unwrap();
        let ancient_ts = format_rfc3339_millis(now - 40 * DAY_MS);
        for name in ["reflection-run-1", "reflection-run-2"] {
            std::fs::write(
                comp_dir.join(format!("{name}.json")),
                format!(
                    r#"{{"schemaVersion":1,"runId":"{name}","finishedAt":"{ancient_ts}","delivery":{{"status":"consumed"}}}}"#
                ),
            )
            .unwrap();
        }
        let filler_paths: Vec<_> = (0..crate::retention::completions::COMPLETIONS_KEEP_NEWEST)
            .map(|i| {
                let name = format!("reflection-run-{}", 100 + i);
                let path = comp_dir.join(format!("{name}.json"));
                std::fs::write(
                    &path,
                    format!(
                        r#"{{"schemaVersion":1,"runId":"{name}","finishedAt":"{newest_ts}","delivery":{{"status":"consumed"}}}}"#
                    ),
                )
                .unwrap();
                path
            })
            .collect();

        // Dry-run preview must count both run-1 and run-2 completions because the same pass removes both runs.
        let policy = RuntimeRetentionPolicy {
            retention_days: 7,
            abandoned_journal_days: 14,
            discard_unreflected_journals: false,
            completions_days: 30,
            pass_budget_ms: 60_000,
        };
        let other = count_runtime_past_retention_other_stores(&paths, &policy, now, None).unwrap();
        assert_eq!(other.completions, 2);
        for path in filler_paths {
            let _ = std::fs::remove_file(path);
        }

        // Now run with a tight cap on a zero-past-window policy (retention_days=365) so cap handles eviction oldest-first
        let cap_policy = RuntimeRetentionPolicy {
            retention_days: 365,
            abandoned_journal_days: 365,
            discard_unreflected_journals: false,
            completions_days: 365,
            pass_budget_ms: 60_000,
        };
        let clock = Cell::new(now);
        let now_fn = || clock.get();
        let report = run_runtime_retention(&RuntimeRetentionOptions {
            context: RetentionContext {
                paths: &paths,
                policy: cap_policy,
                now_ms: &now_fn,
                is_live_session: None,
                reflection_enabled: true,
                aborted: None,
                warn: None,
                before_claim_rename: None,
                before_journal_claim: None,
                before_tombstone_delete: None,
            },
            owned_sidecars: &BTreeSet::new(),
            max_bytes: Some(1500),
            journal_floor_ms: None,
            custom_journal_lock: None,
        })
        .unwrap();

        assert_eq!(
            report.journals.kept.get("corrupt-session"),
            Some(&RetentionKeepReason::StateUnreadable)
        );
        let cap = report.cap.unwrap();
        assert!(
            cap.evicted
                .iter()
                .any(|e| e.tier == RuntimeCapTier::FoldedRun && e.name == "reflection-run-1")
        );
        assert!(!paths.reflection.join("runs/reflection-run-1").exists());
        assert!(paths.reflection.join("runs/reflection-run-3").exists());

        // Check capStalled behavior in stamp.rs
        write_retention_resume_points(
            &paths.runtime,
            &RetentionResumePoints {
                cursor: None,
                cap: Some(RuntimeCapCursor {
                    store: "cap".to_owned(),
                    tier: "folded-run".to_owned(),
                    clock: 100,
                    last_name: "reflection-run-1".to_owned(),
                }),
                cap_stalled: Some(true),
            },
        )
        .unwrap();
        assert!(!retention_resume_pending(&paths.runtime));
    }
}
