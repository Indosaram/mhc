//! Runtime retention pass & host sweep step driven by maintenance (`memory-retention-pass.ts`).

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use memory_core::git::GitMemoryRepo;
use memory_core::identity::layout::MemoryIdentityPaths;
use memory_core::reflection::host_sweep::{
    HostSweepOptions, HostSweepResult, sweep_host_identities,
};
use memory_core::retention::{
    DEFAULT_RUNTIME_MAX_BYTES, DEFAULT_RUNTIME_RETENTION, RETENTION_STAMP_CONFIG_KEY,
    RetentionContext, RetentionHostSweepStepOptions, RuntimeRetentionOptions,
    RuntimeRetentionPolicy, RuntimeRetentionReport, create_retention_host_sweep_step,
    run_runtime_retention, summarize_runtime_retention,
};
use serde_json::Value;

use crate::memory_runtime_retention::{
    resolve_runtime_max_bytes, resolve_runtime_retention_policy,
};

pub use memory_core::retention::retention_resume_pending;

pub type RetentionInfoLog<'a> = &'a dyn Fn(&str);
pub type RetentionWarnLog<'a> = &'a dyn Fn(&str);

pub struct MaintenanceRetentionInput<'a> {
    pub identity: &'a str,
    pub paths: &'a MemoryIdentityPaths,
    pub repo: &'a GitMemoryRepo,
    pub aborted: &'a AtomicBool,
    pub now_ms: i64,
    pub policy: Option<RuntimeRetentionPolicy>,
    pub max_bytes: Option<u64>,
    pub settings: Option<&'a Value>,
    pub reflection_enabled: bool,
    pub is_live_session: Option<&'a dyn Fn(&str) -> bool>,
    pub owned_sidecars: &'a BTreeSet<String>,
    pub info: Option<RetentionInfoLog<'a>>,
    pub warn: Option<RetentionWarnLog<'a>>,
}

/// Runs one identity's retention pass and stamps `omo.retentionAt` when complete or cap-stalled.
pub fn run_maintenance_retention(
    input: MaintenanceRetentionInput<'_>,
) -> Result<RuntimeRetentionReport, String> {
    let policy = input.policy.unwrap_or_else(|| match input.settings {
        Some(settings) => resolve_runtime_retention_policy(settings, input.identity),
        None => DEFAULT_RUNTIME_RETENTION,
    });
    let max_bytes = input.max_bytes.unwrap_or_else(|| match input.settings {
        Some(settings) => resolve_runtime_max_bytes(settings, input.identity),
        None => DEFAULT_RUNTIME_MAX_BYTES,
    });
    let live_fallback = |_: &str| false;
    let is_live_session = input.is_live_session.unwrap_or(&live_fallback);
    let aborted_flag = input.aborted;
    let aborted = move || aborted_flag.load(Ordering::Acquire);
    let now_val = input.now_ms;
    let now_fn = move || now_val;
    let warn_cb = |category: &str, detail: &str| {
        if let Some(warn) = input.warn {
            warn(&format!(
                "{category}: {detail} (identity={})",
                input.identity
            ));
        }
    };

    let report = run_runtime_retention(&RuntimeRetentionOptions {
        context: RetentionContext {
            paths: input.paths,
            policy,
            now_ms: &now_fn,
            reflection_enabled: input.reflection_enabled,
            is_live_session: Some(is_live_session),
            aborted: Some(&aborted),
            warn: Some(&warn_cb),
            before_claim_rename: None,
            before_journal_claim: None,
            before_tombstone_delete: None,
        },
        owned_sidecars: input.owned_sidecars,
        max_bytes: Some(max_bytes),
        journal_floor_ms: None,
        custom_journal_lock: None,
    })?;

    let summary = summarize_runtime_retention(&report);
    if (summary.removed > 0 || !report.skipped.is_empty() || report.cursor.is_some())
        && let Some(info) = input.info
    {
        info(&format!(
            "omo-senpi memory runtime retention identity={} removed={} skipped={:?} cursor={:?}",
            input.identity, summary.removed, report.skipped, report.cursor
        ));
    }

    // A pass whose stores finished and whose cap could not even measure is stamped too: its cap
    // resume point is kept for the next due pass at the interval (#12538 review r5 L1).
    let cap_stalled = report
        .cap
        .as_ref()
        .is_some_and(|cap| cap.skipped.iter().any(|item| item == "measure-over-budget"));
    if report.cursor.is_none() || cap_stalled {
        input
            .repo
            .config_set(RETENTION_STAMP_CONFIG_KEY, &input.now_ms.to_string())
            .map_err(|error| error.to_string())?;
    }

    if let Some(cap) = &report.cap
        && cap.exceeded_bytes > 0
        && let Some(warn) = input.warn
    {
        let mut unfinished = Vec::new();
        if cap.cursor.is_some() {
            unfinished.push("budget");
        }
        for skipped in &cap.skipped {
            unfinished.push(skipped.as_str());
        }
        let message = if unfinished.is_empty() {
            "omo-senpi memory runtime cap exceeded, nothing evictable"
        } else {
            "omo-senpi memory runtime cap exceeded, eviction continues next pass"
        };
        warn(&format!(
            "{message} identity={} overCapBytes={} reason={:?} kept={:?}",
            input.identity, cap.exceeded_bytes, unfinished, cap.kept
        ));
    }

    Ok(report)
}

const HOST_SWEEP_IDENTITIES_PER_PASS: usize = 25;
const HOST_SWEEP_SLICE_MS: i64 = 30_000;

pub struct MaintenanceHostSweepInput<'a> {
    pub memory_root: &'a Path,
    pub settings: Option<&'a Value>,
    pub is_hosted_identity: std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>,
    pub aborted: &'a AtomicBool,
    pub now_ms: i64,
    pub info: Option<RetentionInfoLog<'a>>,
}

pub fn run_maintenance_host_sweep(input: MaintenanceHostSweepInput<'_>) -> Result<(), String> {
    let deadline = input.now_ms.saturating_add(HOST_SWEEP_SLICE_MS);
    let now_val = input.now_ms;
    let now_fn = move || now_val;
    let now_arc: std::sync::Arc<dyn Fn() -> i64 + Send + Sync> =
        std::sync::Arc::new(move || now_val);
    let aborted_flag = input.aborted;
    let aborted = move || aborted_flag.load(Ordering::Acquire);
    let step = create_retention_host_sweep_step(RetentionHostSweepStepOptions {
        settings: input.settings.cloned(),
        is_hosted_identity: Some(input.is_hosted_identity),
        now_ms: Some(now_arc),
        aborted: None,
        deadline_ms: Some(deadline),
        dry_run: false,
    });
    let result = sweep_host_identities(&HostSweepOptions {
        memory_root: input.memory_root.to_path_buf(),
        steps: vec![step],
        max_identities: Some(HOST_SWEEP_IDENTITIES_PER_PASS),
        budget_ms: Some(HOST_SWEEP_SLICE_MS),
        now_ms: Some(&now_fn),
        aborted: Some(&aborted),
        dry_run: false,
    })?;
    if let HostSweepResult::Swept {
        visited,
        completed,
        lines,
    } = result
        && !lines.is_empty()
        && let Some(info) = input.info
    {
        info(&format!(
            "omo-senpi memory host sweep visited={} completed={} lines={:?}",
            visited.len(),
            completed,
            lines
        ));
    }
    Ok(())
}
