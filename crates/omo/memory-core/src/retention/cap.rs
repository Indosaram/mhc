//! Per-identity runtime size cap with 6-tier oldest-first eviction (design 3, #10144, #12538).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::identity::layout::MemoryIdentityPaths;
use crate::journal::lock::JournalLock;
use crate::reflection::run_id_high_water::{reflection_run_number, scan_reflection_run_high_water};

use super::guard::{
    ReflectionRetentionGuard, read_reflection_retention_guard, read_reservations, reserved_run_ids,
};
use super::journals::{
    JournalClass, JournalVerdict, classify_journal_entry, remove_transcript_journal_under_lock,
};
use super::policy::{
    DAY_MS, RetentionContext, RetentionKeepReason, RuntimeRetentionPolicy, cutoff_for_days,
    list_names, newest_mtime,
};
use super::reflection_health_signal::read_unreflected_retention_gate;
use super::reflection_runs::{
    RETENTION_TOMBSTONE_PREFIX, RunClassification, claim_reflection_run, classify_reflection_run,
    remove_tombstone,
};
use super::sidecars::{
    KIBITZER_SIDECAR_TOMBSTONE_PREFIX, claim_kibitzer_sidecar, is_valid_encoded_session,
    kibitzer_sidecar_owner_lock_path_for, kibitzer_sidecars_root,
};
use super::size::{
    RuntimeSizeBudgetOutcome, format_runtime_bytes, measure_runtime_size,
    measure_runtime_size_within_budget, measure_tree_bytes,
};

const RUNTIME_CAP_SIDECAR_FLOOR_MS: i64 = 60 * 60 * 1000;
const RUNTIME_CAP_JOURNAL_FLOOR_MS: i64 = 24 * 60 * 60 * 1000;
const RUNTIME_CAP_KEPT_UNFOLDED_RUNS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeCapTier {
    FoldedRun,
    FullyReflectedTranscript,
    Sidecar,
    NeverReflectedJournal,
    PartlyReflectedTranscript,
    UnfoldedRun,
}

impl RuntimeCapTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FoldedRun => "folded-run",
            Self::FullyReflectedTranscript => "fully-reflected-transcript",
            Self::Sidecar => "sidecar",
            Self::NeverReflectedJournal => "never-reflected-journal",
            Self::PartlyReflectedTranscript => "partly-reflected-transcript",
            Self::UnfoldedRun => "unfolded-run",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeCapKeepReason {
    Reserved,
    ReflectionUnhealthy,
    UnreflectedRetentionOff,
    HighWater,
    Newest,
    NewestUnfolded,
    Locked,
    Recent,
    Live,
    Owned,
    ChangedDuringRemoval,
    StateUnreadable,
}

impl RuntimeCapKeepReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::ReflectionUnhealthy => "reflection-unhealthy",
            Self::UnreflectedRetentionOff => "unreflected-retention-off",
            Self::HighWater => "high-water",
            Self::Newest => "newest",
            Self::NewestUnfolded => "newest-unfolded",
            Self::Locked => "locked",
            Self::Recent => "recent",
            Self::Live => "live",
            Self::Owned => "owned",
            Self::ChangedDuringRemoval => "changed-during-removal",
            Self::StateUnreadable => "state-unreadable",
        }
    }

    fn from_retention(reason: RetentionKeepReason) -> Self {
        match reason {
            RetentionKeepReason::Reserved => Self::Reserved,
            RetentionKeepReason::ReflectionUnhealthy => Self::ReflectionUnhealthy,
            RetentionKeepReason::UnreflectedRetentionOff => Self::UnreflectedRetentionOff,
            RetentionKeepReason::Anchor => Self::HighWater,
            RetentionKeepReason::Newest => Self::Newest,
            RetentionKeepReason::Locked => Self::Locked,
            RetentionKeepReason::Live => Self::Live,
            RetentionKeepReason::Owned => Self::Owned,
            RetentionKeepReason::ChangedDuringRemoval => Self::ChangedDuringRemoval,
            RetentionKeepReason::StateUnreadable => Self::StateUnreadable,
            _ => Self::Recent,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCapCursor {
    pub store: String,
    pub tier: String,
    pub clock: i64,
    pub last_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCapEviction {
    pub tier: RuntimeCapTier,
    pub name: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeSizeCapReport {
    pub max_bytes: u64,
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub exceeded_bytes: u64,
    pub evicted: Vec<RuntimeCapEviction>,
    pub kept: BTreeMap<String, RuntimeCapKeepReason>,
    pub skipped: Vec<String>,
    pub suspended_since: Option<String>,
    pub cursor: Option<RuntimeCapCursor>,
    pub measure_ms: u64,
    pub unreflected_held_bytes: u64,
}

pub struct RuntimeSizeCapOptions<'a> {
    pub context: &'a RetentionContext<'a>,
    pub max_bytes: u64,
    pub owned_sidecars: &'a BTreeSet<String>,
    pub custom_journal_lock: Option<&'a dyn JournalLock>,
    pub journal_floor_ms: Option<i64>,
    pub budget_ms: Option<u64>,
    pub clock_ms: Option<&'a dyn Fn() -> i64>,
    pub measure_aborted: Option<&'a dyn Fn() -> bool>,
    pub resume: Option<RuntimeCapCursor>,
}

struct Candidate<'a> {
    tier: RuntimeCapTier,
    name: String,
    at: i64,
    measured_path: PathBuf,
    evict: Box<dyn FnOnce() -> Result<Option<RuntimeCapKeepReason>, String> + 'a>,
}

fn is_before_cursor(
    tier: RuntimeCapTier,
    at: i64,
    name: &str,
    cursor: Option<&RuntimeCapCursor>,
) -> bool {
    let Some(cursor) = cursor else {
        return false;
    };
    if tier.as_str() != cursor.tier {
        return false;
    }
    at < cursor.clock || (at == cursor.clock && name <= cursor.last_name.as_str())
}

fn clear_tombstones(context: &RetentionContext<'_>) {
    let runs_dir = context.paths.reflection.join("runs");
    for name in list_names(&runs_dir) {
        if name.starts_with(RETENTION_TOMBSTONE_PREFIX) {
            let _ = remove_tombstone(&runs_dir.join(name), context);
        }
    }
    let sidecars_dir = kibitzer_sidecars_root(&context.paths.recall);
    for name in list_names(&sidecars_dir) {
        if name.starts_with(KIBITZER_SIDECAR_TOMBSTONE_PREFIX) {
            let _ = remove_tombstone(&sidecars_dir.join(name), context);
        }
    }
}

pub fn run_runtime_size_cap(
    options: &RuntimeSizeCapOptions<'_>,
) -> Result<RuntimeSizeCapReport, String> {
    if options.context.is_aborted() {
        return Err("Retention aborted".to_owned());
    }
    let context = options.context;
    let default_clock = || crate::support::time::now_millis();
    let clock = options.clock_ms.unwrap_or(&default_clock);
    let mut kept: BTreeMap<String, RuntimeCapKeepReason> = BTreeMap::new();
    let mut evicted: Vec<RuntimeCapEviction> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    clear_tombstones(context);

    let measure_start = clock();
    let before_outcome = measure_runtime_size_within_budget(
        &context.paths.runtime,
        options.budget_ms,
        context.now(),
        options.measure_aborted,
    );
    let before_bytes = match before_outcome {
        RuntimeSizeBudgetOutcome::Size(sz) => sz.bytes,
        RuntimeSizeBudgetOutcome::Unknown(_) => {
            if options.context.is_aborted() {
                return Err("Retention aborted".to_owned());
            }
            skipped.push("measure-over-budget".to_owned());
            return Ok(RuntimeSizeCapReport {
                max_bytes: options.max_bytes,
                before_bytes: 0,
                after_bytes: 0,
                exceeded_bytes: 0,
                evicted,
                kept,
                skipped,
                suspended_since: None,
                cursor: options.resume.clone(),
                measure_ms: u64::try_from(clock().saturating_sub(measure_start).max(0))
                    .unwrap_or(0),
                unreflected_held_bytes: 0,
            });
        }
    };
    let measure_end = clock();
    let measure_ms = u64::try_from(measure_end.saturating_sub(measure_start).max(0)).unwrap_or(0);

    if options.budget_ms.is_some_and(|b| measure_ms > b) {
        skipped.push("measure-over-budget".to_owned());
        return Ok(RuntimeSizeCapReport {
            max_bytes: options.max_bytes,
            before_bytes,
            after_bytes: before_bytes,
            exceeded_bytes: before_bytes.saturating_sub(options.max_bytes),
            evicted,
            kept,
            skipped,
            suspended_since: None,
            cursor: options.resume.clone(),
            measure_ms,
            unreflected_held_bytes: 0,
        });
    }

    if options.max_bytes == 0 || before_bytes <= options.max_bytes {
        let exceeded = if options.max_bytes > 0 {
            before_bytes.saturating_sub(options.max_bytes)
        } else {
            0
        };
        return Ok(RuntimeSizeCapReport {
            max_bytes: options.max_bytes,
            before_bytes,
            after_bytes: before_bytes,
            exceeded_bytes: exceeded,
            evicted,
            kept,
            skipped,
            suspended_since: None,
            cursor: None,
            measure_ms,
            unreflected_held_bytes: 0,
        });
    }

    let guard = read_reflection_retention_guard(context.paths)?;
    if guard.is_none() {
        skipped.push("reflection-scheduler-busy".to_owned());
    }
    let run_guard = guard.clone().unwrap_or_else(|| ReflectionRetentionGuard {
        high_water: scan_reflection_run_high_water(
            &crate::reflection::run_id_high_water::RunIdHighWaterPaths {
                reflection: context.paths.reflection.clone(),
                reflection_sessions: context.paths.reflection_sessions.clone(),
                worktrees: context.paths.worktrees.clone(),
            },
        ),
        reserved: reserved_run_ids(&context.paths.reflection),
        reserved_conversation_ids: BTreeSet::new(),
    });

    let deadline = options
        .budget_ms
        .map(|b| measure_end.saturating_add(i64::try_from(b).unwrap_or(i64::MAX)));
    let mut bytes = before_bytes;
    let mut last: Option<(RuntimeCapTier, i64, String)> = None;
    let mut cursor: Option<RuntimeCapCursor> = None;
    let mut suspended_since: Option<String> = None;
    let mut held_bytes: u64 = 0;

    let (folded_runs, unfolded_runs) = run_candidates(context, &run_guard, &mut kept);
    let mut stopped_early = false;

    macro_rules! walk_tier {
        ($candidates:expr) => {
            if !stopped_early && bytes > options.max_bytes {
                for candidate in $candidates {
                    if bytes <= options.max_bytes {
                        break;
                    }
                    if is_before_cursor(
                        candidate.tier,
                        candidate.at,
                        &candidate.name,
                        options.resume.as_ref(),
                    ) {
                        continue;
                    }
                    if context.is_aborted() {
                        return Err("Retention aborted".to_owned());
                    }
                    if deadline.is_some_and(|d| clock() >= d) {
                        cursor = match &last {
                            Some((tier, at, name)) => Some(RuntimeCapCursor {
                                store: "cap".to_owned(),
                                tier: tier.as_str().to_owned(),
                                clock: *at,
                                last_name: name.clone(),
                            }),
                            None => options.resume.clone(),
                        };
                        stopped_early = true;
                        break;
                    }
                    let size = measure_tree_bytes(&candidate.measured_path, None).unwrap_or(0);
                    let tier = candidate.tier;
                    let at = candidate.at;
                    let name = candidate.name.clone();
                    match (candidate.evict)()? {
                        None => {
                            bytes = bytes.saturating_sub(size);
                            evicted.push(RuntimeCapEviction {
                                tier,
                                name: name.clone(),
                                bytes: size,
                            });
                        }
                        Some(reason) => {
                            kept.insert(name.clone(), reason);
                        }
                    }
                    last = Some((tier, at, name));
                }
            }
        };
    }

    // Tier 1: folded reflection runs
    walk_tier!(folded_runs);

    // Lazily classify journals & sidecars only if still above max_bytes
    if !stopped_early && bytes > options.max_bytes {
        let journals = guard.as_ref().map(|g| {
            journal_candidates(
                context,
                &g.reserved_conversation_ids,
                options.journal_floor_ms,
                options.custom_journal_lock,
                &mut kept,
            )
        });

        // Tier 2: fully-reflected transcripts
        if let Some(ref j) = journals {
            let mut fully = Vec::new();
            for item in &j.fully {
                let dir = context.paths.transcripts.join(&item.name);
                let gate = j.real_gate.as_ref();
                let jctx = &j.tier_context;
                let lock = options.custom_journal_lock;
                fully.push(Candidate {
                    tier: RuntimeCapTier::FullyReflectedTranscript,
                    name: item.name.clone(),
                    at: item.at,
                    measured_path: dir.join("transcript.jsonl"),
                    evict: Box::new(move || {
                        let res = remove_transcript_journal_under_lock(jctx, &dir, gate, lock)
                            .unwrap_or(Err(RetentionKeepReason::StateUnreadable));
                        Ok(res.err().map(RuntimeCapKeepReason::from_retention))
                    }),
                });
            }
            walk_tier!(fully);
        }

        // Tier 3: idle sidecars
        if !stopped_early && bytes > options.max_bytes {
            let sidecars = sidecar_candidates(context, options.owned_sidecars, &mut kept);
            walk_tier!(sidecars);
        }

        // Tiers 4 & 5: never-reflected and partly-reflected journals (health-gated + switch-gated)
        if !stopped_early
            && bytes > options.max_bytes
            && let Some(ref j) = journals
        {
            let hold_reason = if !context.policy.discard_unreflected_journals {
                Some(RuntimeCapKeepReason::UnreflectedRetentionOff)
            } else if let Some(ref gate) = j.real_gate {
                if gate.open {
                    None
                } else {
                    if let Some(newest) = &gate.newest {
                        suspended_since = Some(newest.at.clone());
                    }
                    Some(RuntimeCapKeepReason::ReflectionUnhealthy)
                }
            } else {
                Some(RuntimeCapKeepReason::ReflectionUnhealthy)
            };

            let build_unfolded_journal = |tier: RuntimeCapTier,
                                          items: &[JournalItem],
                                          transcript_only: bool,
                                          kept: &mut BTreeMap<String, RuntimeCapKeepReason>,
                                          held_bytes: &mut u64|
             -> Vec<Candidate<'_>> {
                if let Some(reason) = hold_reason {
                    for item in items {
                        kept.insert(item.name.clone(), reason);
                        let p = if transcript_only {
                            context
                                .paths
                                .transcripts
                                .join(&item.name)
                                .join("transcript.jsonl")
                        } else {
                            context.paths.transcripts.join(&item.name)
                        };
                        *held_bytes =
                            held_bytes.saturating_add(measure_tree_bytes(&p, None).unwrap_or(0));
                    }
                    return Vec::new();
                }
                items
                    .iter()
                    .map(|item| {
                        let dir = context.paths.transcripts.join(&item.name);
                        let measured = if transcript_only {
                            dir.join("transcript.jsonl")
                        } else {
                            dir.clone()
                        };
                        let jctx = &j.tier_context;
                        let gate = j.real_gate.as_ref();
                        let lock = options.custom_journal_lock;
                        Candidate {
                            tier,
                            name: item.name.clone(),
                            at: item.at,
                            measured_path: measured,
                            evict: Box::new(move || {
                                let res =
                                    remove_transcript_journal_under_lock(jctx, &dir, gate, lock)
                                        .unwrap_or(Err(RetentionKeepReason::StateUnreadable));
                                Ok(res.err().map(RuntimeCapKeepReason::from_retention))
                            }),
                        }
                    })
                    .collect()
            };

            let never = build_unfolded_journal(
                RuntimeCapTier::NeverReflectedJournal,
                &j.never,
                false,
                &mut kept,
                &mut held_bytes,
            );
            walk_tier!(never);

            if !stopped_early && bytes > options.max_bytes {
                let partly = build_unfolded_journal(
                    RuntimeCapTier::PartlyReflectedTranscript,
                    &j.partly,
                    true,
                    &mut kept,
                    &mut held_bytes,
                );
                walk_tier!(partly);
            }
        }

        // Tier 6: unfolded runs
        if !stopped_early && bytes > options.max_bytes {
            walk_tier!(unfolded_runs);
        }
        let _ = stopped_early;
    }

    let after_bytes = if evicted.is_empty() {
        before_bytes
    } else {
        measure_runtime_size(&context.paths.runtime, None)
            .map(|s| s.bytes)
            .unwrap_or(bytes)
    };

    Ok(RuntimeSizeCapReport {
        max_bytes: options.max_bytes,
        before_bytes,
        after_bytes,
        exceeded_bytes: after_bytes.saturating_sub(options.max_bytes),
        evicted,
        kept,
        skipped,
        suspended_since,
        cursor,
        measure_ms,
        unreflected_held_bytes: held_bytes,
    })
}

fn run_candidates<'a>(
    context: &'a RetentionContext<'a>,
    guard: &ReflectionRetentionGuard,
    kept: &mut BTreeMap<String, RuntimeCapKeepReason>,
) -> (Vec<Candidate<'a>>, Vec<Candidate<'a>>) {
    let runs_dir = context.paths.reflection.join("runs");
    let mut folded: Vec<(String, i64)> = Vec::new();
    let mut unfolded: Vec<(String, i64)> = Vec::new();
    for name in list_names(&runs_dir) {
        if name.starts_with(RETENTION_TOMBSTONE_PREFIX) {
            continue;
        }
        let run_dir = runs_dir.join(&name);
        match classify_reflection_run(&run_dir, &name) {
            RunClassification::Folded(r) => folded.push((r.name, r.finished_at)),
            RunClassification::Keep(RetentionKeepReason::UnfoldedTerminal) => {
                unfolded.push((name, newest_mtime(&run_dir).unwrap_or(0)));
            }
            RunClassification::Keep(_) => {}
        }
    }
    folded.sort_by(|l, r| l.1.cmp(&r.1).then_with(|| l.0.cmp(&r.0)));
    unfolded.sort_by(|l, r| l.1.cmp(&r.1).then_with(|| l.0.cmp(&r.0)));

    let exempt = |name: &str| -> Option<RuntimeCapKeepReason> {
        if reflection_run_number(name) == guard.high_water {
            Some(RuntimeCapKeepReason::HighWater)
        } else if guard.reserved.contains(name) {
            Some(RuntimeCapKeepReason::Reserved)
        } else {
            None
        }
    };

    let newest_folded = folded.last().map(|r| r.0.clone());
    let mut folded_candidates = Vec::new();
    for (name, at) in folded {
        if let Some(reason) = exempt(&name).or_else(|| {
            (newest_folded.as_deref() == Some(name.as_str()))
                .then_some(RuntimeCapKeepReason::Newest)
        }) {
            kept.insert(name, reason);
            continue;
        }
        let runs_dir_owned = runs_dir.clone();
        let name_for_evict = name.clone();
        folded_candidates.push(Candidate {
            tier: RuntimeCapTier::FoldedRun,
            name: name.clone(),
            at,
            measured_path: runs_dir.join(&name),
            evict: Box::new(move || {
                Ok(
                    claim_reflection_run(context, &runs_dir_owned, &name_for_evict, i64::MAX)?
                        .map(RuntimeCapKeepReason::from_retention),
                )
            }),
        });
    }

    let retention_cutoff = cutoff_for_days(context.now(), context.policy.retention_days);
    let kept_unfolded: BTreeSet<String> = unfolded
        .iter()
        .rev()
        .take(RUNTIME_CAP_KEPT_UNFOLDED_RUNS)
        .map(|(n, _)| n.clone())
        .collect();
    let mut unfolded_candidates = Vec::new();
    for (name, at) in unfolded {
        let reason = exempt(&name)
            .or_else(|| {
                kept_unfolded
                    .contains(&name)
                    .then_some(RuntimeCapKeepReason::NewestUnfolded)
            })
            .or_else(|| (at > retention_cutoff).then_some(RuntimeCapKeepReason::Recent));
        if let Some(r) = reason {
            kept.insert(name, r);
            continue;
        }
        let runs_dir_owned = runs_dir.clone();
        let name_for_evict = name.clone();
        unfolded_candidates.push(Candidate {
            tier: RuntimeCapTier::UnfoldedRun,
            name: name.clone(),
            at,
            measured_path: runs_dir.join(&name),
            evict: Box::new(move || {
                Ok(claim_reflection_run(
                    context,
                    &runs_dir_owned,
                    &name_for_evict,
                    retention_cutoff,
                )?
                .map(RuntimeCapKeepReason::from_retention))
            }),
        });
    }

    (folded_candidates, unfolded_candidates)
}

#[derive(Clone)]
struct JournalItem {
    name: String,
    at: i64,
}

struct JournalScan<'a> {
    fully: Vec<JournalItem>,
    never: Vec<JournalItem>,
    partly: Vec<JournalItem>,
    real_gate: Option<super::reflection_health_signal::UnreflectedRetentionGate>,
    tier_context: RetentionContext<'a>,
}

fn journal_candidates<'a>(
    context: &'a RetentionContext<'a>,
    reserved: &BTreeSet<String>,
    journal_floor_ms: Option<i64>,
    _custom_lock: Option<&dyn JournalLock>,
    kept: &mut BTreeMap<String, RuntimeCapKeepReason>,
) -> JournalScan<'a> {
    let floor_ms = journal_floor_ms.unwrap_or(RUNTIME_CAP_JOURNAL_FLOOR_MS);
    let floor_days = u64::try_from((floor_ms / DAY_MS).max(0)).unwrap_or(1);
    let tier_policy = RuntimeRetentionPolicy {
        retention_days: if journal_floor_ms.is_none() {
            0
        } else {
            floor_days
        },
        abandoned_journal_days: floor_days,
        discard_unreflected_journals: context.policy.discard_unreflected_journals,
        completions_days: context.policy.completions_days,
        pass_budget_ms: context.policy.pass_budget_ms,
    };
    let tier_context = RetentionContext {
        paths: context.paths,
        policy: tier_policy.clone(),
        now_ms: context.now_ms,
        is_live_session: context.is_live_session,
        reflection_enabled: context.reflection_enabled,
        aborted: context.aborted,
        warn: context.warn,
        before_claim_rename: context.before_claim_rename,
        before_journal_claim: context.before_journal_claim,
        before_tombstone_delete: context.before_tombstone_delete,
    };
    let classify_context = RetentionContext {
        paths: context.paths,
        policy: RuntimeRetentionPolicy {
            discard_unreflected_journals: true,
            ..tier_policy
        },
        now_ms: context.now_ms,
        is_live_session: context.is_live_session,
        reflection_enabled: context.reflection_enabled,
        aborted: context.aborted,
        warn: context.warn,
        before_claim_rename: context.before_claim_rename,
        before_journal_claim: context.before_journal_claim,
        before_tombstone_delete: context.before_tombstone_delete,
    };
    let mut admit = || true;
    let real_gate = read_unreflected_retention_gate(
        &context.paths.reflection,
        context.reflection_enabled,
        &mut admit,
    );
    let open_gate = super::reflection_health_signal::UnreflectedRetentionGate {
        open: true,
        newest: None,
    };

    let mut fully = Vec::new();
    let mut never = Vec::new();
    let mut partly = Vec::new();
    for session_id in list_names(&context.paths.transcripts) {
        let dir = context.paths.transcripts.join(&session_id);
        if context
            .is_live_session
            .is_some_and(|live| live(&session_id))
        {
            kept.insert(session_id, RuntimeCapKeepReason::Live);
            continue;
        }
        if reserved.contains(&session_id) {
            kept.insert(session_id, RuntimeCapKeepReason::Reserved);
            continue;
        }
        match classify_journal_entry(&classify_context, &dir, Some(&open_gate)) {
            None => continue,
            Some(JournalVerdict::Keep(RetentionKeepReason::StateUnreadable)) => {
                kept.insert(session_id, RuntimeCapKeepReason::StateUnreadable);
            }
            Some(JournalVerdict::Keep(_)) => {}
            Some(JournalVerdict::Prune(kind)) => {
                let item = JournalItem {
                    name: session_id,
                    at: newest_mtime(&dir).unwrap_or(0),
                };
                match kind {
                    JournalClass::Fully => fully.push(item),
                    JournalClass::Never => never.push(item),
                    JournalClass::Partly => partly.push(item),
                }
            }
        }
    }
    for list in [&mut fully, &mut never, &mut partly] {
        list.sort_by(|l, r| l.at.cmp(&r.at).then_with(|| l.name.cmp(&r.name)));
    }
    JournalScan {
        fully,
        never,
        partly,
        real_gate,
        tier_context,
    }
}

fn sidecar_candidates<'a>(
    context: &'a RetentionContext<'a>,
    owned_sidecars: &BTreeSet<String>,
    kept: &mut BTreeMap<String, RuntimeCapKeepReason>,
) -> Vec<Candidate<'a>> {
    let root = kibitzer_sidecars_root(&context.paths.recall);
    let mut entries = Vec::new();
    let now = context.now();
    for name in list_names(&root) {
        if name.starts_with(KIBITZER_SIDECAR_TOMBSTONE_PREFIX) || !is_valid_encoded_session(&name) {
            continue;
        }
        if owned_sidecars.contains(&name) {
            kept.insert(name, RuntimeCapKeepReason::Owned);
            continue;
        }
        let path = root.join(&name);
        if !path.is_dir() {
            continue;
        }
        let at = newest_mtime(&path).unwrap_or(0);
        if now - at < RUNTIME_CAP_SIDECAR_FLOOR_MS {
            kept.insert(name, RuntimeCapKeepReason::Recent);
            continue;
        }
        entries.push((name, at));
    }
    entries.sort_by(|l, r| l.1.cmp(&r.1).then_with(|| l.0.cmp(&r.0)));
    entries
        .into_iter()
        .map(|(name, at)| {
            let dir = root.join(&name);
            let lock_path = kibitzer_sidecar_owner_lock_path_for(&context.paths.locks, &name);
            Candidate {
                tier: RuntimeCapTier::Sidecar,
                name,
                at,
                measured_path: dir.clone(),
                evict: Box::new(move || {
                    Ok(claim_kibitzer_sidecar(
                        &dir,
                        &lock_path,
                        now,
                        RUNTIME_CAP_SIDECAR_FLOOR_MS,
                        context.warn,
                        context.before_claim_rename,
                        context.before_tombstone_delete,
                    )
                    .map(RuntimeCapKeepReason::from_retention))
                }),
            }
        })
        .collect()
}

pub struct RuntimeCapEvictableOptions<'a> {
    pub paths: &'a MemoryIdentityPaths,
    pub policy: RuntimeRetentionPolicy,
    pub now_ms: &'a dyn Fn() -> i64,
    pub reflection_enabled: bool,
    pub journal_floor_ms: Option<i64>,
    pub aborted: Option<&'a dyn Fn() -> bool>,
}

/// Lock-free upper-bound estimate of bytes evictable under the size cap for doctor commands.
pub fn measure_runtime_cap_evictable_bytes(
    options: &RuntimeCapEvictableOptions<'_>,
) -> Result<u64, String> {
    let reservations = read_reservations(&options.paths.reflection);
    let guard = ReflectionRetentionGuard {
        high_water: scan_reflection_run_high_water(
            &crate::reflection::run_id_high_water::RunIdHighWaterPaths {
                reflection: options.paths.reflection.clone(),
                reflection_sessions: options.paths.reflection_sessions.clone(),
                worktrees: options.paths.worktrees.clone(),
            },
        ),
        reserved: reservations.run_ids,
        reserved_conversation_ids: reservations.conversation_ids,
    };
    let context = RetentionContext {
        paths: options.paths,
        policy: options.policy.clone(),
        now_ms: options.now_ms,
        is_live_session: None,
        reflection_enabled: options.reflection_enabled,
        aborted: options.aborted,
        warn: None,
        before_claim_rename: None,
        before_journal_claim: None,
        before_tombstone_delete: None,
    };
    let mut kept = BTreeMap::new();
    let (folded_runs, unfolded_runs) = run_candidates(&context, &guard, &mut kept);
    let j = journal_candidates(
        &context,
        &guard.reserved_conversation_ids,
        options.journal_floor_ms,
        None,
        &mut kept,
    );
    let empty_owned = BTreeSet::new();
    let sidecars = sidecar_candidates(&context, &empty_owned, &mut kept);

    let mut total = 0u64;
    let measure = |path: &Path| -> Result<u64, String> {
        measure_tree_bytes(path, options.aborted).map_err(|_| "Measurement aborted".to_owned())
    };

    for c in folded_runs
        .iter()
        .chain(sidecars.iter())
        .chain(unfolded_runs.iter())
    {
        total = total.saturating_add(measure(&c.measured_path)?);
    }
    for item in &j.fully {
        total = total.saturating_add(measure(
            &options
                .paths
                .transcripts
                .join(&item.name)
                .join("transcript.jsonl"),
        )?);
    }
    if context.policy.discard_unreflected_journals && j.real_gate.is_some_and(|g| g.open) {
        for item in &j.never {
            total = total.saturating_add(measure(&options.paths.transcripts.join(&item.name))?);
        }
        for item in &j.partly {
            total = total.saturating_add(measure(
                &options
                    .paths
                    .transcripts
                    .join(&item.name)
                    .join("transcript.jsonl"),
            )?);
        }
    }
    Ok(total)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeCapExcess {
    pub over_cap_bytes: u64,
    pub evictable_bytes: u64,
    pub unreflected_bytes: u64,
    pub discard_unreflected_journals: bool,
}

/// Formats the over-cap advisory line shared by `/doctor` and CLI doctor.
pub fn describe_runtime_cap_excess(input: RuntimeCapExcess) -> Option<String> {
    if input.over_cap_bytes == 0 {
        return None;
    }
    let over = format!(
        "cap exceeded by {}",
        format_runtime_bytes(input.over_cap_bytes)
    );
    let held = (!input.discard_unreflected_journals && input.unreflected_bytes > 0).then(|| {
        format!(
            "held by unreflected journals: {} (discard_unreflected_journals off)",
            format_runtime_bytes(input.unreflected_bytes)
        )
    });
    if input.evictable_bytes >= input.over_cap_bytes {
        return Some(format!(
            "{over}; the next maintenance pass evicts it ({} evictable)",
            format_runtime_bytes(input.evictable_bytes)
        ));
    }
    if input.evictable_bytes > 0 {
        let suffix = held.map(|h| format!("; {h}")).unwrap_or_default();
        return Some(format!(
            "{over}; the next maintenance pass evicts up to {} of it{suffix}",
            format_runtime_bytes(input.evictable_bytes)
        ));
    }
    if let Some(h) = held.as_ref()
        && input.over_cap_bytes <= input.unreflected_bytes
    {
        return Some(format!("{over}, nothing evictable; {h}"));
    }
    let suffix = held.map(|h| format!("; {h}")).unwrap_or_default();
    Some(format!(
        "{over}, nothing evictable: live sessions, reserved runs and the newest runs are never evicted{suffix}"
    ))
}
