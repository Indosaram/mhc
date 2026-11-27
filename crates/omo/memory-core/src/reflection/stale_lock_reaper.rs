//! Stale reflection-lock reaper shared by session start, each turn, and `doctor --fix` (#11976, #11920, #12517).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Map, Value};

use crate::fs::remove_tree::remove_tree;
use crate::locks::acquire::{AcquireLockError, AcquireLockOptions, WithLockError, with_lock};
use crate::locks::dead_owner_sweep::{DeadOwnerSweepOptions, sweep_dead_owner_locks};
use crate::locks::domains::{reflection_scheduler_lock_path, run_finalization_lock_path};
use crate::locks::lock_record::create_lock_record;
use crate::locks::process_identity::ProcessLiveness;
use crate::receipts::receipts::{MemoryReceiptInput, append_memory_receipt_once};
use crate::support::time::{format_rfc3339_millis, now_millis, parse_rfc3339};

use super::abandoned::{AbandonedLauncher, AbandonedReason, AbandonedRecord, write_abandoned};
use super::machine::ReservedRun;
use super::orphan_sweep::REFLECTION_ORPHAN_GRACE_MS;
use super::quarantine::{
    QuarantineReason, QuarantineRecord, RESERVATION_EVIDENCE_FILENAME, write_quarantine,
};
use super::release_without_promotion::{
    ReleaseWithoutPromotionOptions, release_reservation_without_promotion,
};
use super::reservation::{is_reserved_run_value, move_torn_reservation_aside};
use super::run_id_high_water::{
    RunIdHighWaterPaths, record_reflection_run_id_mark, reflection_run_number,
    scan_reflection_run_high_water,
};
use super::run_ledger::parse_reservation_run_ledger;
use super::run_process::{
    RecordedProcessSeams, RecordedProcessVerdict, classify_recorded_process,
    classify_recorded_process_lenient,
};
use super::stale_holder::{
    LockReadError, ReflectionLockProbe, ReflectionLockVerdict, SystemReflectionLockProbe,
    classify_reflection_lock_holder, reflection_launcher_liveness,
};
use super::terminal_claim::{
    RunTerminalClaimKind, RunTerminalClaimSeams, RunTerminalPublication, claim_run_terminal,
    read_run_terminal_publication, run_terminal_claim_matches,
};

pub const REFLECTION_LAUNCH_WINDOW_MS: i64 = 60_000;
pub const STRANDED_RUN_GRACE_MS: i64 =
    REFLECTION_LAUNCH_WINDOW_MS + (REFLECTION_ORPHAN_GRACE_MS as i64);
pub const TORN_LOCK_MIN_AGE_MS: i64 = 60 * 60_000;

const TERMINAL_SENTINELS: &[&str] = &["final.json", "abandoned.json", "quarantined.json"];
const TORN_LOCK_TOMBSTONE_PREFIX: &str = ".prune-";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleLockReapAction {
    LaunchInterrupted,
    ProcessesDead,
    LedgerUnreadable,
    LockTorn,
    TerminalRecorded,
    ReleaseSettled,
}

impl StaleLockReapAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LaunchInterrupted => "launch-interrupted",
            Self::ProcessesDead => "processes-dead",
            Self::LedgerUnreadable => "ledger-unreadable",
            Self::LockTorn => "lock-torn",
            Self::TerminalRecorded => "terminal-recorded",
            Self::ReleaseSettled => "release-settled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleLockDecision {
    None,
    Finalize {
        run_id: String,
    },
    Stuck {
        run_id: String,
        reason: String,
    },
    Reap {
        action: StaleLockReapAction,
        run_id: String,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrandedRunAction {
    NoLedger,
    ProcessesDead,
    LedgerUnreadable,
}

impl StrandedRunAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoLedger => "no-ledger",
            Self::ProcessesDead => "processes-dead",
            Self::LedgerUnreadable => "ledger-unreadable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrandedRunDecision {
    Keep {
        reason: String,
    },
    Reap {
        action: StrandedRunAction,
        reason: String,
    },
}

pub trait StaleLockProbe: ReflectionLockProbe {
    fn now_ms(&self) -> i64 {
        now_millis()
    }

    fn list_directory(&self, path: &Path) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(path) else {
            return Vec::new();
        };
        let mut dirs: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        dirs.sort();
        dirs
    }

    fn modified_at_ms(&self, path: &Path) -> Option<i64> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta.modified().ok()?;
        let dur = mtime.duration_since(UNIX_EPOCH).ok()?;
        i64::try_from(dur.as_millis()).ok()
    }
}

pub struct SystemStaleLockProbe;

impl ReflectionLockProbe for SystemStaleLockProbe {
    fn read_text(&self, path: &Path) -> Result<String, LockReadError> {
        SystemReflectionLockProbe.read_text(path)
    }

    fn hostname(&self) -> String {
        SystemReflectionLockProbe.hostname()
    }

    fn pid_liveness(&self, pid: u32) -> ProcessLiveness {
        SystemReflectionLockProbe.pid_liveness(pid)
    }

    fn process_start_identity(&self, pid: u32) -> Result<Option<String>, String> {
        SystemReflectionLockProbe.process_start_identity(pid)
    }
}

impl StaleLockProbe for SystemStaleLockProbe {}

enum TextRead {
    Missing,
    Unreadable,
    Text(String),
}

fn read_text(path: &Path, probe: &dyn StaleLockProbe) -> TextRead {
    match probe.read_text(path) {
        Ok(text) => TextRead::Text(text),
        Err(err) if err.code == "ENOENT" => TextRead::Missing,
        Err(_) => TextRead::Unreadable,
    }
}

fn parse_json(text: &str) -> Option<Value> {
    serde_json::from_str::<Value>(text).ok()
}

fn fields_of(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

fn file_exists(path: &Path, probe: &dyn StaleLockProbe) -> bool {
    !matches!(read_text(path, probe), TextRead::Missing)
}

fn has_terminal_sentinel(run_dir: &Path, probe: &dyn StaleLockProbe) -> bool {
    TERMINAL_SENTINELS
        .iter()
        .any(|name| file_exists(&run_dir.join(name), probe))
}

fn read_final_outcome(run_dir: &Path, probe: &dyn StaleLockProbe) -> Option<String> {
    match read_text(&run_dir.join("final.json"), probe) {
        TextRead::Missing => None,
        TextRead::Unreadable => Some("unknown".to_owned()),
        TextRead::Text(raw) => {
            let outcome = parse_json(&raw)
                .and_then(|v| v.get("outcome").and_then(Value::as_str).map(str::to_owned));
            Some(outcome.unwrap_or_else(|| "unknown".to_owned()))
        }
    }
}

enum SettledSentinelMatch {
    Match,
    OtherGeneration,
}

fn settled_sentinel(
    run_dir: &Path,
    run_id: &str,
    active: &Map<String, Value>,
    probe: &dyn StaleLockProbe,
) -> Option<SettledSentinelMatch> {
    for name in ["abandoned.json", "quarantined.json"] {
        let raw = match read_text(&run_dir.join(name), probe) {
            TextRead::Missing => continue,
            TextRead::Unreadable => return Some(SettledSentinelMatch::OtherGeneration),
            TextRead::Text(text) => text,
        };
        let Some(sentinel) = parse_json(&raw).and_then(|v| v.as_object().cloned()) else {
            return Some(SettledSentinelMatch::OtherGeneration);
        };
        if sentinel.get("runId").and_then(Value::as_str) != Some(run_id) {
            return Some(SettledSentinelMatch::OtherGeneration);
        }
        let Some(gen_str) = sentinel.get("generation").and_then(Value::as_str) else {
            return Some(SettledSentinelMatch::OtherGeneration);
        };
        let generation = parse_rfc3339(gen_str);
        let reserved_at = active
            .get("reservedAt")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339);
        let ledger = read_ledger(run_dir, probe);
        let started_at = ledger
            .fields
            .as_ref()
            .and_then(|f| f.get("startedAt"))
            .and_then(Value::as_str);
        if matches!((generation, reserved_at), (Some(g), Some(r)) if g >= r)
            || started_at == Some(gen_str)
        {
            return Some(SettledSentinelMatch::Match);
        }
        return Some(SettledSentinelMatch::OtherGeneration);
    }
    None
}

/// True when `active.lock`'s launcher is dead on this host and its run already recorded a sentinel
/// for this generation, so only releasing the reservation remains (DESIGN.r3 13b c.14).
pub fn is_settled_under_dead_launcher(reflection_dir: &Path, probe: &dyn StaleLockProbe) -> bool {
    let (_, decision) = decide_stale_reflection_lock(reflection_dir, probe);
    matches!(
        decision,
        StaleLockDecision::Reap {
            action: StaleLockReapAction::ReleaseSettled,
            ..
        }
    )
}

enum RecordedProcesses {
    Dead,
    Live { reason: String },
}

fn process_seams(
    probe: &dyn StaleLockProbe,
) -> (
    impl Fn(u32) -> ProcessLiveness + '_,
    impl Fn(u32) -> Option<String> + '_,
) {
    let liveness = move |pid: u32| probe.pid_liveness(pid);
    let start = move |pid: u32| probe.process_start_identity(pid).ok().flatten();
    (liveness, start)
}

fn recorded_processes(
    ledger: &Map<String, Value>,
    probe: &dyn StaleLockProbe,
    lenient: bool,
) -> RecordedProcesses {
    let (pid_fn, start_fn) = process_seams(probe);
    let seams = RecordedProcessSeams {
        pid_liveness: Some(&pid_fn),
        process_start: Some(&start_fn),
    };
    for (role, pid_key, start_key) in [
        ("supervisor", "pid", "processStart"),
        ("child", "childPid", "childProcessStart"),
    ] {
        let pid = ledger
            .get(pid_key)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok());
        let recorded_start = match ledger.get(start_key) {
            None => None,
            Some(Value::Null) => Some(None),
            Some(Value::String(s)) => Some(Some(s.as_str())),
            Some(_) => None,
        };
        let verdict = if lenient {
            classify_recorded_process_lenient(pid, recorded_start, &seams)
        } else {
            classify_recorded_process(pid, recorded_start, &seams)
        };
        match verdict {
            RecordedProcessVerdict::Alive => {
                return RecordedProcesses::Live {
                    reason: format!("its {role} pid {} is still running", pid.unwrap_or(0)),
                };
            }
            RecordedProcessVerdict::Unknown => {
                return RecordedProcesses::Live {
                    reason: format!("its {role} pid {} could not be checked", pid.unwrap_or(0)),
                };
            }
            RecordedProcessVerdict::Dead | RecordedProcessVerdict::Absent => {}
        }
    }
    RecordedProcesses::Dead
}

struct LedgerRead {
    present: bool,
    fields: Option<Map<String, Value>>,
    valid: bool,
}

fn read_ledger(run_dir: &Path, probe: &dyn StaleLockProbe) -> LedgerRead {
    let raw = match read_text(&run_dir.join("ledger.json"), probe) {
        TextRead::Missing => {
            return LedgerRead {
                present: false,
                fields: None,
                valid: false,
            };
        }
        TextRead::Unreadable => {
            return LedgerRead {
                present: true,
                fields: None,
                valid: false,
            };
        }
        TextRead::Text(text) => text,
    };
    let Some(val) = parse_json(&raw) else {
        return LedgerRead {
            present: true,
            fields: None,
            valid: false,
        };
    };
    let run_id = run_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let valid = parse_reservation_run_ledger(&val)
        .map(|parsed| parsed.run_id == run_id)
        .unwrap_or(false);
    LedgerRead {
        present: true,
        fields: Some(fields_of(&val)),
        valid,
    }
}

fn still_launching(ledger: &LedgerRead, now_ms: i64) -> bool {
    let Some(fields) = &ledger.fields else {
        return false;
    };
    ledger.valid
        && fields.get("launching").and_then(Value::as_bool) == Some(true)
        && fields
            .get("hardDeadlineAt")
            .and_then(Value::as_f64)
            .is_some_and(|d| (now_ms as f64) <= d)
}

const STILL_LAUNCHING: &str =
    "its supervisor is still launching and its hard deadline has not passed";

fn extract_json_string_field(bytes: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let pos = bytes.find(&needle)?;
    let after_key = bytes[pos + needle.len()..].trim_start();
    let after_colon = after_key.strip_prefix(':')?.trim_start();
    let rest = after_colon.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

fn extract_json_u32_field(bytes: &str, key: &str) -> Option<u32> {
    let needle = format!("\"{key}\"");
    let pos = bytes.find(&needle)?;
    let after_key = bytes[pos + needle.len()..].trim_start();
    let after_colon = after_key.strip_prefix(':')?.trim_start();
    let digits: String = after_colon
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

fn extract_torn_run_numbers(bytes: &str) -> Vec<u64> {
    let mut nums = Vec::new();
    let mut rest = bytes;
    while let Some(idx) = rest.find("reflection-run-") {
        let after = &rest[idx + "reflection-run-".len()..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Some(after_digits) = after.get(digits.len()..)
            && !digits.is_empty()
            && after_digits.starts_with('"')
            && let Ok(n) = digits.parse::<u64>()
        {
            nums.push(n);
        }
        rest = after;
    }
    nums
}

fn torn_lock_blocker(
    reflection_dir: &Path,
    bytes: &str,
    probe: &dyn StaleLockProbe,
) -> Option<String> {
    let now = probe.now_ms();
    let lock_at = probe.modified_at_ms(&reflection_dir.join("active.lock"));
    if lock_at.is_none_or(|at| now - at <= TORN_LOCK_MIN_AGE_MS) {
        // rule:torn-recent
        return Some("it is recent".to_owned());
    }
    let host = extract_json_string_field(bytes, "launcherHostname");
    if let Some(ref h) = host
        && h != &probe.hostname()
    {
        // rule:torn-other-host
        return Some(format!("its launcher is on host {h}"));
    }
    let pid = extract_json_u32_field(bytes, "launcherPid");
    if pid.is_some() && host.is_none() {
        // rule:torn-host-unknown
        return Some("its launcher host is unknown".to_owned());
    }
    if let Some(pid) = pid {
        let start = extract_json_string_field(bytes, "launcherProcessStart");
        if reflection_launcher_liveness(pid, start.as_deref(), probe) != ProcessLiveness::Dead {
            // rule:torn-launcher-alive
            return Some(format!("its launcher pid {pid} may still be alive"));
        }
    }
    let runs_dir = reflection_dir.join("runs");
    for name in probe.list_directory(&runs_dir) {
        if name.starts_with('.') {
            continue;
        }
        let run_dir = runs_dir.join(&name);
        if has_terminal_sentinel(&run_dir, probe) {
            continue;
        }
        let at = probe.modified_at_ms(&run_dir);
        if at.is_none_or(|t| (now - t) as f64 <= REFLECTION_ORPHAN_GRACE_MS) {
            // rule:torn-young-run-dir
            return Some(format!("run {name} is a young run dir"));
        }
        let ledger = read_ledger(&run_dir, probe);
        if ledger.present && still_launching(&ledger, now) {
            // rule:torn-launching-run
            return Some(format!("run {name} is still launching"));
        }
        if let Some(fields) = &ledger.fields
            && matches!(
                recorded_processes(fields, probe, false),
                RecordedProcesses::Live { .. }
            )
        {
            // rule:torn-live-run
            return Some(format!("run {name} may still be live"));
        }
    }
    None
}

fn torn_lock(
    reflection_dir: &Path,
    bytes: &str,
    reason: &str,
    probe: &dyn StaleLockProbe,
) -> StaleLockDecision {
    if let Some(blocker) = torn_lock_blocker(reflection_dir, bytes, probe) {
        return StaleLockDecision::Stuck {
            run_id: "unknown".to_owned(),
            reason: format!("{reason} and {blocker}"),
        };
    }
    // rule:lock-torn
    StaleLockDecision::Reap {
        action: StaleLockReapAction::LockTorn,
        run_id: "unknown".to_owned(),
        reason: reason.to_owned(),
    }
}

fn decide_stuck(
    reflection_dir: &Path,
    run_id: &str,
    verdict_reason: &str,
    probe: &dyn StaleLockProbe,
) -> StaleLockDecision {
    let pass_through = || StaleLockDecision::Stuck {
        run_id: run_id.to_owned(),
        reason: verdict_reason.to_owned(),
    };
    let TextRead::Text(lock_text) = read_text(&reflection_dir.join("active.lock"), probe) else {
        return pass_through();
    };
    let Some(parsed) = parse_json(&lock_text) else {
        return torn_lock(
            reflection_dir,
            &lock_text,
            "active.lock is not valid JSON",
            probe,
        );
    };
    let fields = fields_of(&parsed);
    let launcher_pid = fields
        .get("launcherPid")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok());
    let launcher_host = fields.get("launcherHostname").and_then(Value::as_str);
    let (Some(launcher_pid), Some(launcher_host)) = (launcher_pid, launcher_host) else {
        if !is_reserved_run_value(&parsed) {
            return torn_lock(
                reflection_dir,
                &lock_text,
                "active.lock is not a reservation",
                probe,
            );
        }
        // rule:legacy-lock
        return StaleLockDecision::Stuck {
            run_id: run_id.to_owned(),
            reason: "its launcher identity is missing; a session reconciles it".to_owned(),
        };
    };
    if launcher_host != probe.hostname() {
        return pass_through();
    }
    let start = fields.get("launcherProcessStart").and_then(Value::as_str);
    if reflection_launcher_liveness(launcher_pid, start, probe) != ProcessLiveness::Dead {
        return pass_through();
    }
    let run_dir = reflection_dir.join("runs").join(run_id);
    if file_exists(&run_dir.join("outcome.json"), probe) {
        return pass_through();
    }
    if !is_reserved_run_value(&parsed) {
        return torn_lock(
            reflection_dir,
            &lock_text,
            "active.lock is not a reservation",
            probe,
        );
    }
    if let Some(final_outcome) = read_final_outcome(&run_dir, probe) {
        if final_outcome == "merged" || final_outcome == "no_changes" {
            // rule:final-folded
            return StaleLockDecision::Stuck {
                run_id: run_id.to_owned(),
                reason: "it finished with a folded outcome that only a session can settle; open a session".to_owned(),
            };
        }
        // rule:terminal-recorded
        return StaleLockDecision::Reap {
            action: StaleLockReapAction::TerminalRecorded,
            run_id: run_id.to_owned(),
            reason: "the run already recorded its terminal state".to_owned(),
        };
    }
    match settled_sentinel(&run_dir, run_id, &fields, probe) {
        Some(SettledSentinelMatch::Match) => {
            // rule:release-settled
            return StaleLockDecision::Reap {
                action: StaleLockReapAction::ReleaseSettled,
                run_id: run_id.to_owned(),
                reason: "it was already settled and only the release is missing".to_owned(),
            };
        }
        Some(SettledSentinelMatch::OtherGeneration) => {
            // rule:settled-other-generation
            return StaleLockDecision::Stuck {
                run_id: run_id.to_owned(),
                reason: "its run dir holds a terminal record from another generation".to_owned(),
            };
        }
        None => {}
    }
    let ledger = read_ledger(&run_dir, probe);
    if !ledger.present {
        let reserved_at = fields
            .get("reservedAt")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339);
        if let Some(r) = reserved_at
            && probe.now_ms() - r <= REFLECTION_LAUNCH_WINDOW_MS
        {
            // rule:launch-window
            return StaleLockDecision::Stuck {
                run_id: run_id.to_owned(),
                reason: "its launcher is gone but the launch window has not closed".to_owned(),
            };
        }
        // rule:launch-interrupted
        return StaleLockDecision::Reap {
            action: StaleLockReapAction::LaunchInterrupted,
            run_id: run_id.to_owned(),
            reason: format!("launcher pid {launcher_pid} is gone and the run never started"),
        };
    }
    if still_launching(&ledger, probe.now_ms()) {
        // rule:run-launching
        return StaleLockDecision::Stuck {
            run_id: run_id.to_owned(),
            reason: STILL_LAUNCHING.to_owned(),
        };
    }
    if let Some(fields) = &ledger.fields
        && let RecordedProcesses::Live { reason } = recorded_processes(fields, probe, !ledger.valid)
    {
        // rule:run-process-alive
        return StaleLockDecision::Stuck {
            run_id: run_id.to_owned(),
            reason,
        };
    }
    if ledger.valid {
        // rule:processes-dead
        return StaleLockDecision::Reap {
            action: StaleLockReapAction::ProcessesDead,
            run_id: run_id.to_owned(),
            reason: "its launcher, supervisor and child are all gone and it recorded no outcome"
                .to_owned(),
        };
    }
    // rule:ledger-unreadable
    StaleLockDecision::Reap {
        action: StaleLockReapAction::LedgerUnreadable,
        run_id: run_id.to_owned(),
        reason: "its ledger cannot be read and its recorded processes are gone".to_owned(),
    }
}

/// Fail-closed decision table over `reflection_dir/active.lock` and its `runs/<runId>` evidence.
pub fn decide_stale_reflection_lock(
    reflection_dir: &Path,
    probe: &dyn StaleLockProbe,
) -> (ReflectionLockVerdict, StaleLockDecision) {
    let verdict = classify_reflection_lock_holder(reflection_dir, probe);
    let decision = match &verdict {
        ReflectionLockVerdict::None | ReflectionLockVerdict::Live { .. } => StaleLockDecision::None,
        // rule:finalize
        ReflectionLockVerdict::Dead { run_id } => StaleLockDecision::Finalize {
            run_id: run_id.clone(),
        },
        ReflectionLockVerdict::Stuck { run_id, reason } => {
            decide_stuck(reflection_dir, run_id, reason, probe)
        }
    };
    (verdict, decision)
}

/// When a ledger-less run dir was last launched (`prelaunch.json` mtime, else `run_dir` mtime).
pub fn stranded_since(run_dir: &Path, probe: &dyn StaleLockProbe) -> Option<i64> {
    let prelaunch = run_dir.join("prelaunch.json");
    if file_exists(&prelaunch, probe) {
        probe.modified_at_ms(&prelaunch)
    } else {
        probe.modified_at_ms(run_dir)
    }
}

/// Fail-closed classification of a run directory outside the active/pending reservation.
pub fn decide_stranded_run(run_dir: &Path, probe: &dyn StaleLockProbe) -> StrandedRunDecision {
    let ledger = read_ledger(run_dir, probe);
    let now = probe.now_ms();
    if !ledger.present {
        if let TextRead::Text(raw) = read_text(&run_dir.join("prelaunch.json"), probe) {
            let named = parse_json(&raw)
                .and_then(|v| v.get("runId").and_then(Value::as_str).map(str::to_owned));
            let Some(named) = named else {
                // rule:stranded-prelaunch-unreadable
                return StrandedRunDecision::Keep {
                    reason: "its prelaunch.json cannot be read".to_owned(),
                };
            };
            let dir_name = run_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if named != dir_name {
                // rule:stranded-prelaunch-mismatch
                return StrandedRunDecision::Keep {
                    reason: "its prelaunch.json names another run".to_owned(),
                };
            }
        }
        let since = stranded_since(run_dir, probe);
        if since.is_none_or(|t| now - t <= STRANDED_RUN_GRACE_MS) {
            // rule:stranded-young
            return StrandedRunDecision::Keep {
                reason: "it is inside the launch grace window".to_owned(),
            };
        }
        // rule:stranded-no-ledger
        return StrandedRunDecision::Reap {
            action: StrandedRunAction::NoLedger,
            reason: "it never got a ledger".to_owned(),
        };
    }
    if still_launching(&ledger, now) {
        // rule:stranded-launching
        return StrandedRunDecision::Keep {
            reason: STILL_LAUNCHING.to_owned(),
        };
    }
    if let Some(fields) = &ledger.fields
        && let RecordedProcesses::Live { reason } = recorded_processes(fields, probe, !ledger.valid)
    {
        // rule:stranded-process-alive
        return StrandedRunDecision::Keep { reason };
    }
    if ledger.valid {
        // rule:stranded-processes-dead
        return StrandedRunDecision::Reap {
            action: StrandedRunAction::ProcessesDead,
            reason: "its supervisor and child are gone and it recorded no outcome".to_owned(),
        };
    }
    // rule:stranded-ledger-unreadable
    StrandedRunDecision::Reap {
        action: StrandedRunAction::LedgerUnreadable,
        reason: "its ledger cannot be read and its recorded processes are gone".to_owned(),
    }
}

pub type ReceiptAppender<'a> = &'a dyn Fn(&Path, &MemoryReceiptInput) -> Result<bool, String>;

pub struct StaleLockReaperOptions<'a> {
    pub runtime_dir: &'a Path,
    pub probe: Option<&'a dyn StaleLockProbe>,
    /// In-session settle (reconciliation); `None` runs the minimal CLI settle.
    pub settle: Option<&'a mut dyn FnMut() -> Result<(), String>>,
    /// Run `settle` even when `active.lock` needs nothing (session-start reconciliation).
    pub settle_always: bool,
    /// Return `false` if this session already settled this run without releasing it.
    pub should_settle: Option<&'a dyn Fn(&str) -> bool>,
    pub dry_run: bool,
    pub append_receipt: Option<ReceiptAppender<'a>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleLockReaperAction {
    None,
    Settled,
    SettleDeclined,
    Reaped,
    Kept,
    WouldReap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrandedRunReport {
    pub run_id: String,
    pub decision: StrandedRunDecision,
    pub applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstoneReport {
    pub name: String,
    pub removed: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleLockReaperReport {
    pub before: ReflectionLockVerdict,
    pub decision: StaleLockDecision,
    pub action: StaleLockReaperAction,
    pub after: ReflectionLockVerdict,
    pub stranded: Vec<StrandedRunReport>,
    pub tombstones: Vec<TombstoneReport>,
    pub warnings: Vec<String>,
    pub dry_run: bool,
    pub pending_kept: Option<String>,
    pub kept_reason: Option<String>,
}

struct ReaperContext<'a> {
    runtime_dir: &'a Path,
    reflection_dir: PathBuf,
    transcripts_dir: PathBuf,
    locks_dir: PathBuf,
    probe: &'a dyn StaleLockProbe,
    append_receipt: ReceiptAppender<'a>,
    warnings: Vec<String>,
}

fn under_lock<T>(
    lock_path: &Path,
    purpose: &str,
    task: impl FnOnce() -> Result<T, String>,
) -> Result<Option<T>, String> {
    let record = create_lock_record(purpose, Default::default()).map_err(|e| e.to_string())?;
    let opts = AcquireLockOptions {
        wait_timeout_ms: Some(0),
        retry_delay_ms: None,
        cancellation: None,
    };
    match with_lock(lock_path, &record, &opts, task) {
        Ok(val) => Ok(Some(val)),
        Err(WithLockError::Acquire(AcquireLockError::Contention(_))) => Ok(None),
        Err(WithLockError::Acquire(err)) => Err(err.to_string()),
        Err(WithLockError::User(err)) => Err(err),
    }
}

fn emit_receipt(context: &mut ReaperContext<'_>, input: &MemoryReceiptInput) {
    if let Err(err) = (context.append_receipt)(context.runtime_dir, input) {
        let id = match input {
            MemoryReceiptInput::Run { run_id, .. } => run_id.as_str(),
            MemoryReceiptInput::Facts { batch_id, .. } => batch_id.as_str(),
        };
        context
            .warnings
            .push(format!("receipt for {id} not written: {err}"));
    }
}

fn iso_or(value: Option<&Value>, fallback: &str) -> String {
    match value.and_then(Value::as_str) {
        Some(s) if parse_rfc3339(s).is_some() => s.to_owned(),
        _ => fallback.to_owned(),
    }
}

struct RunIdentity {
    kind: String,
    trigger: String,
    origin: Option<String>,
}

fn identity_from_reservation(active: &Map<String, Value>) -> RunIdentity {
    let req = active
        .get("request")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let trigger = req
        .get("trigger")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let origin = req.get("origin").and_then(Value::as_str).map(str::to_owned);
    let kind = if trigger == "dream" {
        "dream".to_owned()
    } else {
        "reflection".to_owned()
    };
    RunIdentity {
        kind,
        trigger,
        origin,
    }
}

fn identity_from_ledger(fields: &Map<String, Value>, fallback: RunIdentity) -> RunIdentity {
    let kind = match fields.get("kind").and_then(Value::as_str) {
        Some("dream") => "dream".to_owned(),
        Some("reflection") => "reflection".to_owned(),
        _ => fallback.kind,
    };
    let trigger = fields
        .get("trigger")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or(fallback.trigger);
    let origin = fields
        .get("origin")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or(fallback.origin);
    RunIdentity {
        kind,
        trigger,
        origin,
    }
}

fn abandon_run(
    context: &mut ReaperContext<'_>,
    run_dir: &Path,
    run_id: &str,
    reason: AbandonedReason,
    generation: &str,
    identity: RunIdentity,
    launcher: Option<&Map<String, Value>>,
) -> Result<(), String> {
    std::fs::create_dir_all(run_dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(run_dir, std::fs::Permissions::from_mode(0o700));
    }
    let launcher_info = launcher.map(|l| AbandonedLauncher {
        pid: l
            .get("launcherPid")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        hostname: l
            .get("launcherHostname")
            .and_then(Value::as_str)
            .map(str::to_owned),
        process_start: l
            .get("launcherProcessStart")
            .and_then(Value::as_str)
            .map(str::to_owned),
    });
    let record = AbandonedRecord {
        version: 1,
        run_id: run_id.to_owned(),
        abandoned_at: format_rfc3339_millis(context.probe.now_ms()),
        reason,
        generation: generation.to_owned(),
        kind: identity.kind.clone(),
        trigger: identity.trigger.clone(),
        origin: identity.origin.clone(),
        launcher: launcher_info,
    };
    write_abandoned(run_dir, &record).map_err(|e| e.to_string())?;
    emit_receipt(
        context,
        &MemoryReceiptInput::Run {
            kind: identity.kind,
            run_id: run_id.to_owned(),
            trigger: identity.origin.unwrap_or(identity.trigger),
            event: "abandoned".to_owned(),
            generation: generation.to_owned(),
            sha: None,
            reason: Some(reason.as_str().to_owned()),
            detail: None,
        },
    );
    Ok(())
}

fn quarantine_run(
    context: &mut ReaperContext<'_>,
    run_dir: &Path,
    run_id: &str,
    generation: &str,
    identity: RunIdentity,
    reservation: Option<&ReservedRun>,
    reason: QuarantineReason,
) -> Result<(), String> {
    let mut evidence: Vec<String> = std::fs::read_dir(run_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    evidence.sort();
    if reservation.is_some() {
        evidence.push(RESERVATION_EVIDENCE_FILENAME.to_owned());
    }
    let record = QuarantineRecord {
        version: 1,
        run_id: run_id.to_owned(),
        kind: identity.kind.clone(),
        trigger: identity.trigger.clone(),
        origin: identity.origin.clone(),
        generation: generation.to_owned(),
        reason,
        quarantined_at: format_rfc3339_millis(context.probe.now_ms()),
        evidence,
    };
    write_quarantine(run_dir, &record, reservation).map_err(|e| e.to_string())?;
    emit_receipt(
        context,
        &MemoryReceiptInput::Run {
            kind: identity.kind,
            run_id: run_id.to_owned(),
            trigger: identity.origin.unwrap_or(identity.trigger),
            event: "quarantined".to_owned(),
            generation: generation.to_owned(),
            sha: None,
            reason: Some(reason.as_str().to_owned()),
            detail: None,
        },
    );
    Ok(())
}

fn reserved_run_id(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let val = parse_json(&raw)?;
    val.get("runId").and_then(Value::as_str).map(str::to_owned)
}

fn read_active_fields(reflection_dir: &Path) -> Option<Map<String, Value>> {
    let raw = std::fs::read_to_string(reflection_dir.join("active.lock")).ok()?;
    parse_json(&raw).and_then(|v| v.as_object().cloned())
}

fn release_reservation(
    context: &ReaperContext<'_>,
    run_id: &str,
) -> Result<(bool, Option<String>), String> {
    let probe = context.probe;
    let now_iso = || format_rfc3339_millis(probe.now_ms());
    match release_reservation_without_promotion(&ReleaseWithoutPromotionOptions {
        reflection_dir: &context.reflection_dir,
        transcripts_dir: &context.transcripts_dir,
        locks_dir: &context.locks_dir,
        run_id,
        now_iso: Some(&now_iso),
    }) {
        Ok(res) => Ok((res.released, res.pending_run_id)),
        Err(err) if err.contains("held by") || err.contains("LockContention") => Ok((false, None)),
        Err(err) => Err(err),
    }
}

enum MinimalSettle {
    Settled {
        pending_run_id: Option<String>,
        already_released: bool,
    },
    NotSettled {
        reason: String,
    },
}

fn settle_active_minimally(
    context: &mut ReaperContext<'_>,
    action: StaleLockReapAction,
    run_id: &str,
) -> Result<MinimalSettle, String> {
    let run_dir = context.reflection_dir.join("runs").join(run_id);
    let lock_path =
        run_finalization_lock_path(&context.locks_dir, run_id).map_err(|e| e.to_string())?;
    let write_res = under_lock(
        &lock_path,
        "reflection-finalize",
        || -> Result<MinimalSettle, String> {
            let (_, again) = decide_stale_reflection_lock(&context.reflection_dir, context.probe);
            match again {
                StaleLockDecision::Reap {
                    action: a,
                    run_id: r,
                    ..
                } if a == action && r == run_id => {}
                _ => {
                    return Ok(MinimalSettle::NotSettled {
                        reason: "the decision changed".to_owned(),
                    });
                }
            }
            let active = read_active_fields(&context.reflection_dir).unwrap_or_default();
            let identity = identity_from_reservation(&active);
            let now_str = format_rfc3339_millis(context.probe.now_ms());
            let reserved_at = iso_or(active.get("reservedAt"), &now_str);

            if action == StaleLockReapAction::LaunchInterrupted {
                abandon_run(
                    context,
                    &run_dir,
                    run_id,
                    AbandonedReason::LaunchInterrupted,
                    &reserved_at,
                    identity,
                    Some(&active),
                )?;
                return Ok(MinimalSettle::Settled {
                    pending_run_id: None,
                    already_released: false,
                });
            }
            if matches!(
                action,
                StaleLockReapAction::TerminalRecorded | StaleLockReapAction::ReleaseSettled
            ) {
                let (released, pending_run_id) = release_reservation(context, run_id)?;
                return Ok(if released {
                    MinimalSettle::Settled {
                        pending_run_id,
                        already_released: true,
                    }
                } else {
                    MinimalSettle::NotSettled {
                        reason: "the reservation changed before it could be released".to_owned(),
                    }
                });
            }
            if !matches!(
                action,
                StaleLockReapAction::ProcessesDead | StaleLockReapAction::LedgerUnreadable
            ) {
                return Ok(MinimalSettle::NotSettled {
                    reason: format!("{} is not settled from here", action.as_str()),
                });
            }

            let sys_probe = SystemStaleLockProbe;
            let ledger = read_ledger(&run_dir, &sys_probe);
            let fields = ledger.fields.unwrap_or_default();
            let attempt = fields.get("attempt").and_then(Value::as_i64);
            if let Some(published) =
                read_run_terminal_publication(&run_dir, attempt).map_err(|e| e.to_string())?
            {
                return Ok(MinimalSettle::NotSettled {
                    reason: format!(
                        "the run is already {}",
                        if published == RunTerminalPublication::Settled {
                            "settled"
                        } else {
                            "publishing its outcome"
                        }
                    ),
                });
            }
            let claim_attempt = attempt.unwrap_or(1);
            let (pid_fn, start_fn) = process_seams(context.probe);
            let claim_seams = RunTerminalClaimSeams {
                process: RecordedProcessSeams {
                    pid_liveness: Some(&pid_fn),
                    process_start: Some(&start_fn),
                },
                before_recovery: None,
            };
            let claim = claim_run_terminal(
                &run_dir,
                run_id,
                claim_attempt,
                RunTerminalClaimKind::Abandon,
                &claim_seams,
                None,
            )
            .map_err(|e| e.to_string())?;
            if !run_terminal_claim_matches(
                &claim,
                run_id,
                claim_attempt,
                Some(RunTerminalClaimKind::Abandon),
            ) {
                return Ok(MinimalSettle::NotSettled {
                    reason: "another process claimed the run".to_owned(),
                });
            }
            let generation = iso_or(fields.get("startedAt"), &reserved_at);
            let run_identity = identity_from_ledger(&fields, identity);
            if action == StaleLockReapAction::ProcessesDead {
                abandon_run(
                    context,
                    &run_dir,
                    run_id,
                    AbandonedReason::ProcessesDead,
                    &generation,
                    run_identity,
                    None,
                )?;
            } else {
                let reservation = serde_json::from_value::<ReservedRun>(Value::Object(active)).ok();
                quarantine_run(
                    context,
                    &run_dir,
                    run_id,
                    &generation,
                    run_identity,
                    reservation.as_ref(),
                    QuarantineReason::LedgerUnreadable,
                )?;
            }
            Ok(MinimalSettle::Settled {
                pending_run_id: None,
                already_released: false,
            })
        },
    )?;

    let Some(settled) = write_res else {
        return Ok(MinimalSettle::NotSettled {
            reason: "a session is finalizing it".to_owned(),
        });
    };
    match settled {
        MinimalSettle::NotSettled { reason } => Ok(MinimalSettle::NotSettled { reason }),
        MinimalSettle::Settled {
            pending_run_id,
            already_released: true,
        } => Ok(MinimalSettle::Settled {
            pending_run_id,
            already_released: true,
        }),
        MinimalSettle::Settled {
            already_released: false,
            ..
        } => {
            let (released, pending_run_id) = release_reservation(context, run_id)?;
            if !released {
                Ok(MinimalSettle::NotSettled {
                    reason: "the reservation changed before it could be released".to_owned(),
                })
            } else {
                Ok(MinimalSettle::Settled {
                    pending_run_id,
                    already_released: true,
                })
            }
        }
    }
}

fn reap_torn_lock(
    context: &mut ReaperContext<'_>,
    reflection_sessions: &Path,
    worktrees: &Path,
) -> Result<bool, String> {
    let lock_path = reflection_scheduler_lock_path(&context.locks_dir);
    let moved = under_lock(
        &lock_path,
        "reflection-scheduler",
        || -> Result<bool, String> {
            let (_, again) = decide_stale_reflection_lock(&context.reflection_dir, context.probe);
            if !matches!(
                again,
                StaleLockDecision::Reap {
                    action: StaleLockReapAction::LockTorn,
                    ..
                }
            ) {
                return Ok(false);
            }
            let bytes = std::fs::read_to_string(context.reflection_dir.join("active.lock"))
                .map_err(|e| e.to_string())?;
            let named = extract_torn_run_numbers(&bytes);
            let scanned = scan_reflection_run_high_water(&RunIdHighWaterPaths {
                reflection: context.reflection_dir.clone(),
                reflection_sessions: reflection_sessions.to_path_buf(),
                worktrees: worktrees.to_path_buf(),
            });
            let high_water = named.iter().copied().fold(scanned, u64::max);
            if high_water > 0 {
                record_reflection_run_id_mark(&context.reflection_dir, high_water)
                    .map_err(|e| e.to_string())?;
            }
            let now_ms = context.probe.now_ms();
            let moved_name = move_torn_reservation_aside(&context.reflection_dir, &bytes, now_ms)
                .map_err(|e| e.to_string())?;
            if moved_name.is_none() {
                return Ok(false);
            }
            let torn_run = named
                .iter()
                .copied()
                .max()
                .map_or_else(|| "unknown".to_owned(), |n| format!("reflection-run-{n}"));
            emit_receipt(
                context,
                &MemoryReceiptInput::Run {
                    kind: "reflection".to_owned(),
                    run_id: torn_run,
                    trigger: "unknown".to_owned(),
                    event: "abandoned".to_owned(),
                    generation: format_rfc3339_millis(now_ms),
                    sha: None,
                    reason: Some("lock_torn".to_owned()),
                    detail: None,
                },
            );
            Ok(true)
        },
    )?;
    Ok(moved == Some(true))
}

fn settle_stranded_run_under_lock(
    context: &mut ReaperContext<'_>,
    run_dir: &Path,
    run_id: &str,
    action: &StrandedRunAction,
) -> Result<bool, String> {
    let sys_probe = SystemStaleLockProbe;
    let since = stranded_since(run_dir, &sys_probe);
    let lock_path =
        run_finalization_lock_path(&context.locks_dir, run_id).map_err(|e| e.to_string())?;
    let applied = under_lock(
        &lock_path,
        "reflection-finalize",
        || -> Result<bool, String> {
            if has_terminal_sentinel(run_dir, &sys_probe) {
                return Ok(false);
            }
            let ledger = read_ledger(run_dir, &sys_probe);
            let fields = ledger.fields.unwrap_or_default();
            let unknown = || RunIdentity {
                kind: "reflection".to_owned(),
                trigger: "unknown".to_owned(),
                origin: None,
            };
            let fallback = format_rfc3339_millis(since.unwrap_or_else(|| context.probe.now_ms()));
            match action {
                StrandedRunAction::NoLedger => {
                    quarantine_run(
                        context,
                        run_dir,
                        run_id,
                        &fallback,
                        unknown(),
                        None,
                        QuarantineReason::StrandedNoLedger,
                    )?;
                }
                StrandedRunAction::ProcessesDead => {
                    let generation = iso_or(fields.get("startedAt"), &fallback);
                    abandon_run(
                        context,
                        run_dir,
                        run_id,
                        AbandonedReason::ProcessesDead,
                        &generation,
                        identity_from_ledger(&fields, unknown()),
                        None,
                    )?;
                }
                StrandedRunAction::LedgerUnreadable => {
                    let generation = iso_or(fields.get("startedAt"), &fallback);
                    quarantine_run(
                        context,
                        run_dir,
                        run_id,
                        &generation,
                        identity_from_ledger(&fields, unknown()),
                        None,
                        QuarantineReason::LedgerUnreadable,
                    )?;
                }
            }
            Ok(true)
        },
    )?;
    Ok(applied == Some(true))
}

pub struct QuarantineStrandedRunOptions<'a> {
    pub runtime_dir: &'a Path,
    pub run_dir: &'a Path,
    pub run_id: &'a str,
    pub action: StrandedRunAction,
    pub now_ms: Option<&'a dyn Fn() -> i64>,
    pub append_receipt: Option<ReceiptAppender<'a>>,
}

/// Settles one stranded run dir outside `active.lock`/`pending.json` under its finalize lock.
pub fn quarantine_stranded_run(options: &QuarantineStrandedRunOptions<'_>) -> Result<bool, String> {
    let sys_probe = SystemStaleLockProbe;
    struct CustomNowProbe<'a>(&'a dyn Fn() -> i64);
    impl ReflectionLockProbe for CustomNowProbe<'_> {
        fn read_text(&self, path: &Path) -> Result<String, LockReadError> {
            SystemReflectionLockProbe.read_text(path)
        }
        fn hostname(&self) -> String {
            SystemReflectionLockProbe.hostname()
        }
        fn pid_liveness(&self, pid: u32) -> ProcessLiveness {
            SystemReflectionLockProbe.pid_liveness(pid)
        }
        fn process_start_identity(&self, pid: u32) -> Result<Option<String>, String> {
            SystemReflectionLockProbe.process_start_identity(pid)
        }
    }
    impl StaleLockProbe for CustomNowProbe<'_> {
        fn now_ms(&self) -> i64 {
            (self.0)()
        }
    }
    let custom;
    let probe: &dyn StaleLockProbe = match options.now_ms {
        Some(f) => {
            custom = CustomNowProbe(f);
            &custom
        }
        None => &sys_probe,
    };
    let mut context = ReaperContext {
        runtime_dir: options.runtime_dir,
        reflection_dir: options.runtime_dir.join("reflection"),
        transcripts_dir: options.runtime_dir.join("transcripts"),
        locks_dir: options.runtime_dir.join("locks"),
        probe,
        append_receipt: options
            .append_receipt
            .unwrap_or(&append_memory_receipt_once),
        warnings: Vec::new(),
    };
    settle_stranded_run_under_lock(
        &mut context,
        options.run_dir,
        options.run_id,
        &options.action,
    )
}

fn stranded_runs(
    context: &mut ReaperContext<'_>,
    apply: bool,
) -> Result<Vec<StrandedRunReport>, String> {
    let mut reserved = std::collections::BTreeSet::new();
    for name in ["active.lock", "pending.json"] {
        if let Some(id) = reserved_run_id(&context.reflection_dir.join(name)) {
            reserved.insert(id);
        }
    }
    let runs_dir = context.reflection_dir.join("runs");
    let sys_probe = SystemStaleLockProbe;
    let mut reports = Vec::new();
    for run_id in sys_probe.list_directory(&runs_dir) {
        if run_id.starts_with('.') || reserved.contains(&run_id) {
            continue;
        }
        let run_dir = runs_dir.join(&run_id);
        if has_terminal_sentinel(&run_dir, &sys_probe) {
            continue;
        }
        let decision = decide_stranded_run(&run_dir, context.probe);
        match &decision {
            StrandedRunDecision::Keep { .. } => {
                reports.push(StrandedRunReport {
                    run_id,
                    decision,
                    applied: false,
                });
            }
            StrandedRunDecision::Reap { action, .. } => {
                let applied = if apply {
                    settle_stranded_run_under_lock(context, &run_dir, &run_id, action)?
                } else {
                    false
                };
                reports.push(StrandedRunReport {
                    run_id,
                    decision,
                    applied,
                });
            }
        }
    }
    Ok(reports)
}

fn remove_tombstones(context: &ReaperContext<'_>, apply: bool) -> Vec<TombstoneReport> {
    let runs_dir = context.reflection_dir.join("runs");
    let Ok(entries) = std::fs::read_dir(&runs_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.starts_with(TORN_LOCK_TOMBSTONE_PREFIX))
        .collect();
    names.sort();

    let mut reports = Vec::new();
    for name in names {
        if !apply {
            reports.push(TombstoneReport {
                name,
                removed: false,
                error: None,
            });
            continue;
        }
        match remove_tree(&runs_dir.join(&name)) {
            Ok(()) => reports.push(TombstoneReport {
                name,
                removed: true,
                error: None,
            }),
            Err(err) => reports.push(TombstoneReport {
                name,
                removed: false,
                error: Some(err.to_string()),
            }),
        }
    }
    reports
}

/// Runs the stale-lock reaper once over `runtime_dir`.
pub fn run_stale_lock_reaper(
    mut options: StaleLockReaperOptions<'_>,
) -> Result<StaleLockReaperReport, String> {
    let sys_probe = SystemStaleLockProbe;
    let probe = options.probe.unwrap_or(&sys_probe);
    let mut context = ReaperContext {
        runtime_dir: options.runtime_dir,
        reflection_dir: options.runtime_dir.join("reflection"),
        transcripts_dir: options.runtime_dir.join("transcripts"),
        locks_dir: options.runtime_dir.join("locks"),
        probe,
        append_receipt: options
            .append_receipt
            .unwrap_or(&append_memory_receipt_once),
        warnings: Vec::new(),
    };
    let reflection_sessions = options.runtime_dir.join("reflection-sessions");
    let worktrees = options.runtime_dir.join("worktrees");

    let (before, decision) = decide_stale_reflection_lock(&context.reflection_dir, probe);
    let minimal = options.settle.is_none();
    let action;
    let mut pending_kept: Option<String> = None;
    let mut kept_reason: Option<String> = None;

    if options.dry_run {
        match &decision {
            StaleLockDecision::Reap { .. } => {
                action = StaleLockReaperAction::WouldReap;
                pending_kept = reserved_run_id(&context.reflection_dir.join("pending.json"));
            }
            StaleLockDecision::None => action = StaleLockReaperAction::None,
            _ => action = StaleLockReaperAction::Kept,
        }
    } else {
        match &decision {
            StaleLockDecision::Reap {
                action: StaleLockReapAction::LockTorn,
                ..
            } => {
                action = if reap_torn_lock(&mut context, &reflection_sessions, &worktrees)? {
                    StaleLockReaperAction::Reaped
                } else {
                    StaleLockReaperAction::Kept
                };
            }
            StaleLockDecision::Reap {
                action: reap_action,
                run_id,
                ..
            } => {
                if let Some(settle) = options.settle.as_mut() {
                    if options.should_settle.is_some_and(|check| !check(run_id)) {
                        action = StaleLockReaperAction::SettleDeclined;
                    } else {
                        settle()?;
                        action = StaleLockReaperAction::Settled;
                    }
                } else {
                    match settle_active_minimally(&mut context, *reap_action, run_id)? {
                        MinimalSettle::Settled { pending_run_id, .. } => {
                            action = StaleLockReaperAction::Reaped;
                            pending_kept = pending_run_id;
                        }
                        MinimalSettle::NotSettled { reason } => {
                            action = StaleLockReaperAction::Kept;
                            kept_reason = Some(reason);
                        }
                    }
                }
            }
            StaleLockDecision::Finalize { run_id } => {
                if let Some(settle) = options.settle.as_mut() {
                    if options.should_settle.is_some_and(|check| !check(run_id)) {
                        action = StaleLockReaperAction::SettleDeclined;
                    } else {
                        settle()?;
                        action = StaleLockReaperAction::Settled;
                    }
                } else {
                    action = StaleLockReaperAction::Kept;
                    kept_reason =
                        Some("it recorded its outcome; open a session to finalize it".to_owned());
                }
            }
            StaleLockDecision::None => action = StaleLockReaperAction::None,
            StaleLockDecision::Stuck { .. } => action = StaleLockReaperAction::Kept,
        }
    }

    if !options.dry_run
        && options.settle_always
        && action != StaleLockReaperAction::Settled
        && let Some(settle) = options.settle.as_mut()
    {
        settle()?;
    }

    let stranded = if minimal {
        stranded_runs(&mut context, !options.dry_run)?
    } else {
        Vec::new()
    };
    let tombstones = if minimal {
        remove_tombstones(&context, !options.dry_run)
    } else {
        Vec::new()
    };
    let after = if options.dry_run {
        before.clone()
    } else {
        classify_reflection_lock_holder(&context.reflection_dir, probe)
    };

    let _ = reflection_run_number("reflection-run-0");

    Ok(StaleLockReaperReport {
        before,
        decision,
        action,
        after,
        stranded,
        tombstones,
        warnings: context.warnings,
        dry_run: options.dry_run,
        pending_kept,
        kept_reason,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptLockInfo {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadOwnerLockReport {
    pub reclaimed: Vec<String>,
    pub kept: Vec<KeptLockInfo>,
}

pub fn reclaim_dead_owner_locks(locks_dir: &Path) -> DeadOwnerLockReport {
    let mut names: Vec<String> = std::fs::read_dir(locks_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.ends_with(".lock"))
        .collect();
    names.sort();

    let reclaimed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let failures = std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::<String, String>::new()));
    let r_clone = reclaimed.clone();
    let f_clone = failures.clone();
    sweep_dead_owner_locks(
        locks_dir,
        &DeadOwnerSweepOptions {
            on_reclaimed: Some(std::sync::Arc::new(move |lock_path, _| {
                if let Some(name) = lock_path.file_name().and_then(|n| n.to_str()) {
                    r_clone
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(name.to_owned());
                }
            })),
            on_failure: Some(std::sync::Arc::new(move |lock_path, error| {
                if let Some(name) = lock_path.file_name().and_then(|n| n.to_str()) {
                    f_clone
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(name.to_owned(), error.to_owned());
                }
            })),
        },
    );

    let reclaimed = std::mem::take(&mut *reclaimed.lock().unwrap_or_else(|e| e.into_inner()));
    let failures = std::mem::take(&mut *failures.lock().unwrap_or_else(|e| e.into_inner()));
    let kept = names
        .into_iter()
        .filter(|name| !reclaimed.contains(name))
        .map(|name| KeptLockInfo {
            reason: failures
                .get(&name)
                .cloned()
                .unwrap_or_else(|| "owner is alive or not provably dead".to_owned()),
            name,
        })
        .collect();
    DeadOwnerLockReport { reclaimed, kept }
}

pub fn format_stale_lock_fix_lines(
    identity: &str,
    report: &StaleLockReaperReport,
    locks: Option<&DeadOwnerLockReport>,
) -> Vec<String> {
    let mut lines = Vec::new();
    match &report.decision {
        StaleLockDecision::Reap { action, run_id, .. } => {
            let label = action.as_str();
            append_decision_lines(&mut lines, identity, report, label, run_id, false);
        }
        StaleLockDecision::Finalize { run_id } => {
            append_decision_lines(&mut lines, identity, report, "finalize", run_id, true);
        }
        StaleLockDecision::Stuck { run_id, reason } => {
            lines.push(format!(
                "KEPT {identity} active.lock run {run_id} ({reason})"
            ));
        }
        StaleLockDecision::None => {}
    }

    for entry in &report.stranded {
        let StrandedRunDecision::Reap { action, .. } = &entry.decision else {
            continue;
        };
        let prefix = if entry.applied {
            "FIXED"
        } else if report.dry_run {
            "WOULD FIX"
        } else {
            "KEPT"
        };
        lines.push(format!(
            "{prefix} {identity} run {} ({})",
            entry.run_id,
            action.as_str()
        ));
    }
    for tombstone in &report.tombstones {
        if tombstone.removed {
            lines.push(format!(
                "FIXED {identity} removed tombstone {}",
                tombstone.name
            ));
        } else if let Some(err) = &tombstone.error {
            lines.push(format!(
                "KEPT {identity} tombstone {} ({err})",
                tombstone.name
            ));
        }
    }
    for warning in &report.warnings {
        lines.push(format!("WARN {identity} {warning}"));
    }
    if let Some(locks) = locks {
        let failed: Vec<String> = locks
            .kept
            .iter()
            .filter(|e| e.reason != "owner is alive or not provably dead")
            .map(|e| format!("{}: {}", e.name, e.reason))
            .collect();
        let suffix = if failed.is_empty() {
            String::new()
        } else {
            format!(" ({})", failed.join("; "))
        };
        lines.push(format!(
            "INFO {identity} locks reclaimed {}, kept {}{suffix}",
            locks.reclaimed.len(),
            locks.kept.len()
        ));
    }
    lines
}

fn append_decision_lines(
    lines: &mut Vec<String>,
    identity: &str,
    report: &StaleLockReaperReport,
    label: &str,
    run_id: &str,
    is_finalize: bool,
) {
    let after_run_id = match &report.after {
        ReflectionLockVerdict::Live { run_id }
        | ReflectionLockVerdict::Dead { run_id }
        | ReflectionLockVerdict::Stuck { run_id, .. } => Some(run_id.as_str()),
        ReflectionLockVerdict::None => None,
    };
    let released_by_settle = report.action == StaleLockReaperAction::Settled
        && (matches!(report.after, ReflectionLockVerdict::None)
            || after_run_id.is_some_and(|after_id| after_id != run_id));
    if report.action == StaleLockReaperAction::WouldReap {
        lines.push(format!(
            "WOULD FIX {identity} active.lock run {run_id} ({label})"
        ));
        if let Some(pending) = &report.pending_kept {
            lines.push(format!(
                "INFO {identity} pending {pending} stays queued for the next session"
            ));
        }
    } else if report.action == StaleLockReaperAction::Reaped || released_by_settle {
        lines.push(format!(
            "FIXED {identity} active.lock run {run_id} ({label})"
        ));
        if let Some(pending) = &report.pending_kept {
            lines.push(format!(
                "INFO {identity} pending {pending} kept for the next session"
            ));
        }
    } else if is_finalize && report.action == StaleLockReaperAction::Kept {
        lines.push(format!(
            "KEPT {identity} active.lock run {run_id} (it recorded its outcome; open a session to finalize it)"
        ));
    } else {
        let reason = report
            .kept_reason
            .clone()
            .unwrap_or_else(|| format!("{label} did not release the lock"));
        lines.push(format!(
            "KEPT {identity} active.lock run {run_id} ({reason})"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receipts::receipts::read_memory_receipts;

    #[test]
    fn reaps_interrupted_launch_past_window_keeps_pending_and_writes_abandoned_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path();
        let reflection = runtime.join("reflection");
        let locks = runtime.join("locks");
        std::fs::create_dir_all(&reflection).unwrap();
        std::fs::create_dir_all(&locks).unwrap();

        let active = serde_json::json!({
            "runId": "reflection-run-5",
            "reservedAt": "2026-01-01T00:00:00.000Z",
            "launcherPid": 999_999_991u32,
            "launcherHostname": crate::support::host::hostname(),
            "request": {
                "trigger": "step-count",
                "conversationIds": ["c1"],
                "snapshots": []
            }
        });
        std::fs::write(reflection.join("active.lock"), active.to_string()).unwrap();
        std::fs::write(
            reflection.join("pending.json"),
            r#"{"runId":"reflection-run-6","request":{"trigger":"manual","conversationIds":["c1"],"snapshots":[]}}"#,
        )
        .unwrap();

        let dry = run_stale_lock_reaper(StaleLockReaperOptions {
            runtime_dir: runtime,
            probe: None,
            settle: None,
            settle_always: false,
            should_settle: None,
            dry_run: true,
            append_receipt: None,
        })
        .unwrap();
        assert_eq!(dry.action, StaleLockReaperAction::WouldReap);
        assert_eq!(dry.pending_kept.as_deref(), Some("reflection-run-6"));
        assert!(reflection.join("active.lock").exists());

        let report = run_stale_lock_reaper(StaleLockReaperOptions {
            runtime_dir: runtime,
            probe: None,
            settle: None,
            settle_always: false,
            should_settle: None,
            dry_run: false,
            append_receipt: None,
        })
        .unwrap();
        assert_eq!(report.action, StaleLockReaperAction::Reaped);
        assert_eq!(report.pending_kept.as_deref(), Some("reflection-run-6"));
        assert!(!reflection.join("active.lock").exists());
        assert!(reflection.join("pending.json").exists());
        assert!(
            reflection
                .join("runs/reflection-run-5/abandoned.json")
                .exists()
        );
        let receipts = read_memory_receipts(runtime, &Default::default()).unwrap();
        assert_eq!(receipts.receipts.len(), 1);
    }

    #[test]
    fn quarantines_stranded_run_past_grace_and_removes_tombstones() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path();
        let reflection = runtime.join("reflection");
        let run_dir = reflection.join("runs/reflection-run-8");
        let tomb = reflection.join("runs/.prune-reflection-run-3");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::create_dir_all(&tomb).unwrap();
        std::fs::write(tomb.join("marker"), "x").unwrap();

        struct FarFutureProbe;
        impl ReflectionLockProbe for FarFutureProbe {
            fn read_text(&self, path: &Path) -> Result<String, LockReadError> {
                SystemReflectionLockProbe.read_text(path)
            }
            fn hostname(&self) -> String {
                SystemReflectionLockProbe.hostname()
            }
            fn pid_liveness(&self, pid: u32) -> ProcessLiveness {
                SystemReflectionLockProbe.pid_liveness(pid)
            }
            fn process_start_identity(&self, pid: u32) -> Result<Option<String>, String> {
                SystemReflectionLockProbe.process_start_identity(pid)
            }
        }
        impl StaleLockProbe for FarFutureProbe {
            fn now_ms(&self) -> i64 {
                now_millis() + STRANDED_RUN_GRACE_MS + 10_000
            }
        }

        let probe = FarFutureProbe;
        let report = run_stale_lock_reaper(StaleLockReaperOptions {
            runtime_dir: runtime,
            probe: Some(&probe),
            settle: None,
            settle_always: false,
            should_settle: None,
            dry_run: false,
            append_receipt: None,
        })
        .unwrap();
        assert_eq!(report.stranded.len(), 1);
        assert!(report.stranded[0].applied);
        assert!(run_dir.join("quarantined.json").exists());
        assert!(!tomb.exists());
    }
}
