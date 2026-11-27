//! Host-wide retention sweep step over foreign identities (design 6b, #12532, #12932).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use crate::identity::layout::build_identity_paths;
use crate::reflection::host_sweep::{HostSweepIdentity, HostSweepStep};

use super::cap::{
    RuntimeCapEvictableOptions, RuntimeCapExcess, describe_runtime_cap_excess,
    measure_runtime_cap_evictable_bytes,
};
use super::pass::{RuntimeRetentionOptions, run_runtime_retention, summarize_runtime_retention};
use super::past_retention::{
    count_runtime_past_retention, count_runtime_past_retention_other_stores,
    measure_unreflected_journal_bytes,
};
use super::policy::{DAY_MS, DEFAULT_RUNTIME_RETENTION, RetentionContext, RuntimeRetentionPolicy};
use super::size::{
    DEFAULT_RUNTIME_MAX_BYTES, RuntimeSizeBudgetOutcome, measure_runtime_size_within_budget,
};

fn read_u64_setting(settings: Option<&Value>, identity: &str, key: &str, fallback: u64) -> u64 {
    let Some(root) = settings.and_then(Value::as_object) else {
        return fallback;
    };
    let agent_val = root
        .get("agents")
        .and_then(Value::as_object)
        .and_then(|a| a.get(identity))
        .and_then(Value::as_object)
        .and_then(|a| a.get("runtime"))
        .and_then(Value::as_object)
        .and_then(|r| r.get(key));
    let global_val = root
        .get("runtime")
        .and_then(Value::as_object)
        .and_then(|r| r.get(key));
    for candidate in [agent_val, global_val].into_iter().flatten() {
        if let Some(n) = candidate.as_u64() {
            return n;
        }
        if let Some(f) = candidate.as_f64()
            && f.is_finite()
            && f >= 0.0
        {
            return f as u64;
        }
    }
    fallback
}

/// Resolves `discard_unreflected_journals` for an unhosted identity using only the global config
/// and its `agents.<id>.runtime` override (never another project's layer).
pub fn resolve_foreign_identity_discard_unreflected(
    settings: Option<&Value>,
    identity: &str,
) -> bool {
    let Some(root) = settings.and_then(Value::as_object) else {
        return false;
    };
    let global = root
        .get("runtime")
        .and_then(Value::as_object)
        .and_then(|r| r.get("discard_unreflected_journals"))
        .and_then(Value::as_bool);
    if global != Some(true) {
        return false;
    }
    let agent_override = root
        .get("agents")
        .and_then(Value::as_object)
        .and_then(|a| a.get(identity))
        .and_then(Value::as_object)
        .and_then(|a| a.get("runtime"))
        .and_then(Value::as_object)
        .and_then(|r| r.get("discard_unreflected_journals"))
        .and_then(Value::as_bool);
    agent_override != Some(false)
}

pub fn resolve_foreign_identity_retention_policy(
    settings: Option<&Value>,
    identity: &str,
) -> RuntimeRetentionPolicy {
    RuntimeRetentionPolicy {
        retention_days: read_u64_setting(
            settings,
            identity,
            "retention_days",
            DEFAULT_RUNTIME_RETENTION.retention_days,
        ),
        abandoned_journal_days: read_u64_setting(
            settings,
            identity,
            "abandoned_journal_days",
            DEFAULT_RUNTIME_RETENTION.abandoned_journal_days,
        ),
        completions_days: read_u64_setting(
            settings,
            identity,
            "completions_days",
            DEFAULT_RUNTIME_RETENTION.completions_days,
        ),
        pass_budget_ms: read_u64_setting(
            settings,
            identity,
            "pass_budget_ms",
            DEFAULT_RUNTIME_RETENTION.pass_budget_ms,
        ),
        discard_unreflected_journals: resolve_foreign_identity_discard_unreflected(
            settings, identity,
        ),
    }
}

pub fn resolve_foreign_identity_max_bytes(settings: Option<&Value>, identity: &str) -> u64 {
    read_u64_setting(settings, identity, "max_bytes", DEFAULT_RUNTIME_MAX_BYTES)
}

pub type HostedIdentityPredicate = std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>;

#[derive(Default)]
pub struct RetentionHostSweepStepOptions<'a> {
    pub settings: Option<Value>,
    pub is_hosted_identity: Option<HostedIdentityPredicate>,
    pub now_ms: Option<std::sync::Arc<dyn Fn() -> i64 + Send + Sync>>,
    pub aborted: Option<&'a dyn Fn() -> bool>,
    pub deadline_ms: Option<i64>,
    pub dry_run: bool,
}

fn memory_root_of(identity_root: &Path) -> &Path {
    identity_root
        .parent()
        .and_then(Path::parent)
        .unwrap_or(identity_root)
}

fn preview_retention(
    identity: &str,
    root: &Path,
    policy: &RuntimeRetentionPolicy,
    max_bytes: u64,
    now_fn: &dyn Fn() -> i64,
) -> Result<Vec<String>, String> {
    let paths = build_identity_paths(memory_root_of(root), identity);
    let now = now_fn();
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(policy.pass_budget_ms);
    let timed_out = || std::time::Instant::now() >= deadline;

    let past = count_runtime_past_retention(&paths, policy, now_fn, None, true, Some(&timed_out));
    let other = count_runtime_past_retention_other_stores(&paths, policy, now, Some(&timed_out))?;
    let total = past.runs
        + past.journals
        + past.sidecars
        + other.completions
        + other.ledgers
        + other.legacy;
    let mut lines = if total == 0 {
        Vec::new()
    } else {
        vec![format!(
            "WOULD FIX {identity} retention would remove {total} (runs {}, completions {}, journals {}, sidecars {}, ledgers {}, legacy {})",
            past.runs, other.completions, past.journals, past.sidecars, other.ledgers, other.legacy
        )]
    };

    if max_bytes == 0 {
        return Ok(lines);
    }
    let RuntimeSizeBudgetOutcome::Size(measured) = measure_runtime_size_within_budget(
        &paths.runtime,
        Some(policy.pass_budget_ms),
        now,
        Some(&timed_out),
    ) else {
        return Ok(lines);
    };
    if measured.bytes <= max_bytes {
        return Ok(lines);
    }

    let journal_floor_ms = i64::try_from(policy.retention_days)
        .unwrap_or(i64::MAX)
        .saturating_mul(DAY_MS);
    let evictable_bytes = measure_runtime_cap_evictable_bytes(&RuntimeCapEvictableOptions {
        paths: &paths,
        policy: policy.clone(),
        now_ms: now_fn,
        reflection_enabled: true,
        journal_floor_ms: Some(journal_floor_ms),
        aborted: Some(&timed_out),
    })?;
    let unreflected_bytes = if policy.discard_unreflected_journals {
        0
    } else {
        measure_unreflected_journal_bytes(&paths, policy, Some(&timed_out))?
    };

    if let Some(excess) = describe_runtime_cap_excess(RuntimeCapExcess {
        over_cap_bytes: measured.bytes - max_bytes,
        evictable_bytes,
        unreflected_bytes,
        discard_unreflected_journals: policy.discard_unreflected_journals,
    }) {
        lines.push(format!("WOULD FIX {identity} size cap: {excess}"));
    }
    Ok(lines)
}

pub fn create_retention_host_sweep_step(
    options: RetentionHostSweepStepOptions<'static>,
) -> HostSweepStep {
    let settings = options.settings;
    let is_hosted = options.is_hosted_identity;
    let now_arc = options
        .now_ms
        .unwrap_or_else(|| std::sync::Arc::new(crate::support::time::now_millis));
    let deadline_ms = options.deadline_ms;
    let dry_run = options.dry_run;

    std::sync::Arc::new(move |identity: &HostSweepIdentity| -> Vec<String> {
        if is_hosted
            .as_ref()
            .is_some_and(|hosted| hosted(&identity.name))
        {
            return Vec::new();
        }
        let policy = resolve_foreign_identity_retention_policy(settings.as_ref(), &identity.name);
        let max_bytes = resolve_foreign_identity_max_bytes(settings.as_ref(), &identity.name);
        let now_fn = || (now_arc)();

        if dry_run {
            return match preview_retention(
                &identity.name,
                &identity.root,
                &policy,
                max_bytes,
                &now_fn,
            ) {
                Ok(lines) => lines,
                Err(err) => vec![format!(
                    "WOULD FIX {} retention preview unavailable: {err}",
                    identity.name
                )],
            };
        }

        let remaining_budget = match deadline_ms {
            Some(deadline) => {
                let left = deadline.saturating_sub(now_fn()).max(1) as u64;
                policy.pass_budget_ms.min(left)
            }
            None => policy.pass_budget_ms,
        };
        let journal_floor_ms = i64::try_from(policy.retention_days)
            .unwrap_or(i64::MAX)
            .saturating_mul(DAY_MS);
        let paths = build_identity_paths(memory_root_of(&identity.root), &identity.name);
        let run_policy = RuntimeRetentionPolicy {
            pass_budget_ms: remaining_budget,
            ..policy
        };

        let owned = BTreeSet::new();
        match run_runtime_retention(&RuntimeRetentionOptions {
            context: RetentionContext {
                paths: &paths,
                policy: run_policy,
                now_ms: &now_fn,
                is_live_session: None,
                reflection_enabled: true,
                aborted: None,
                warn: None,
                before_claim_rename: None,
                before_journal_claim: None,
                before_tombstone_delete: None,
            },
            owned_sidecars: &owned,
            max_bytes: Some(max_bytes),
            journal_floor_ms: Some(journal_floor_ms),
            custom_journal_lock: None,
        }) {
            Ok(report) => {
                let summary = summarize_runtime_retention(&report);
                if summary.removed == 0 && report.cursor.is_none() {
                    return Vec::new();
                }
                let recall_suffix =
                    if report.recall_runs == super::legacy::LegacyRecallRunsOutcome::Removed {
                        " + recall/runs"
                    } else {
                        ""
                    };
                let cap_suffix = if summary.cap_evicted > 0 {
                    format!(", size cap {}", summary.cap_evicted)
                } else {
                    String::new()
                };
                let resume_suffix = match &report.cursor {
                    Some(c) => format!("; resumes at {}", c.store_name()),
                    None => String::new(),
                };
                let _unused: BTreeMap<String, usize> = BTreeMap::new();
                vec![format!(
                    "FIXED {} retention removed {} (runs {}, completions {}, journals {}, sidecars {}, ledgers {}, legacy {}{recall_suffix}{cap_suffix}){resume_suffix}",
                    identity.name,
                    summary.removed,
                    summary.runs,
                    summary.completions,
                    summary.journals,
                    summary.sidecars,
                    summary.ledgers,
                    summary.reflection_sessions,
                )]
            }
            Err(err) => vec![format!("FAIL {} retention failed: {err}", identity.name)],
        }
    })
}
