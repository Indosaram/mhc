//! Exclusive durable terminal claims (`terminal-claim.json`) for publishing or abandoning a reflection run.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::locks::acquire::{AcquireLockError, AcquireLockOptions, WithLockError, with_lock};
use crate::locks::lock_record::create_lock_record;
use crate::locks::process_identity::{ProcessLiveness, get_pid_liveness};
use crate::reflection::run_process::RecordedProcessSeams;
use crate::support::random::random_uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunTerminalClaimKind {
    Publish,
    Abandon,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunTerminalClaim {
    pub version: u32,
    pub kind: RunTerminalClaimKind,
    pub run_id: String,
    pub attempt: i64,
    pub claimant_pid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalClaimRecoveryReason {
    Invalid,
    DeadPublish,
}

#[derive(Default, Clone, Copy)]
pub struct RunTerminalClaimSeams<'a> {
    pub process: RecordedProcessSeams<'a>,
    pub before_recovery: Option<&'a dyn Fn(TerminalClaimRecoveryReason)>,
}

#[derive(Debug)]
pub struct RunTerminalClaimUnrecoverableError {
    pub run_dir: PathBuf,
    pub detail: String,
}

impl std::fmt::Display for RunTerminalClaimUnrecoverableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Run terminal claim cannot be recovered in {}: {}",
            self.run_dir.display(),
            self.detail
        )
    }
}

impl std::error::Error for RunTerminalClaimUnrecoverableError {}

pub fn run_terminal_claim_path(run_dir: &Path) -> PathBuf {
    run_dir.join("terminal-claim.json")
}

pub fn run_terminal_claim_matches(
    claim: &RunTerminalClaim,
    run_id: &str,
    attempt: i64,
    kind: Option<RunTerminalClaimKind>,
) -> bool {
    claim.run_id == run_id && claim.attempt == attempt && kind.is_none_or(|k| claim.kind == k)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunTerminalPublication {
    Settled,
    Finalize,
    Veto,
}

/// Reads whether a run already recorded a terminal sentinel (`final.json`/`abandoned.json`/`quarantined.json`),
/// an outcome for `attempt`, or a publication in progress (`publishing.json`).
pub fn read_run_terminal_publication(
    run_dir: &Path,
    attempt: Option<i64>,
) -> std::io::Result<Option<RunTerminalPublication>> {
    for name in ["final.json", "abandoned.json", "quarantined.json"] {
        if run_dir.join(name).exists() {
            return Ok(Some(RunTerminalPublication::Settled));
        }
    }
    if let Some(outcome) = read_json_object(&run_dir.join("outcome.json"))?
        && outcome.get("attempt").and_then(Value::as_i64) == attempt
    {
        return Ok(Some(RunTerminalPublication::Finalize));
    }
    if let Some(publishing) = read_json_object(&run_dir.join("publishing.json"))?
        && publishing.get("version").and_then(Value::as_i64) == Some(1)
        && publishing.get("attempt").and_then(Value::as_i64) == attempt
    {
        return Ok(Some(RunTerminalPublication::Veto));
    }
    Ok(None)
}

pub fn read_run_terminal_claim(run_dir: &Path) -> Result<RunTerminalClaim, String> {
    let raw =
        std::fs::read_to_string(run_terminal_claim_path(run_dir)).map_err(|e| e.to_string())?;
    let claim: RunTerminalClaim = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    if claim.version != 1 || claim.run_id.is_empty() || claim.claimant_pid == 0 {
        return Err("Invalid run terminal claim".to_owned());
    }
    Ok(claim)
}

/// Claims terminal ownership (`publish` or `abandon`) exclusively via `hard_link`, recovering corrupt
/// or dead-publisher `terminal-claim.json` files under `terminal-claim-recovery.lock`.
pub fn claim_run_terminal(
    run_dir: &Path,
    run_id: &str,
    attempt: i64,
    kind: RunTerminalClaimKind,
    seams: &RunTerminalClaimSeams<'_>,
    claimant_pid: Option<u32>,
) -> Result<RunTerminalClaim, RunTerminalClaimUnrecoverableError> {
    let pid = claimant_pid.unwrap_or_else(std::process::id);
    if pid == 0 {
        return Err(RunTerminalClaimUnrecoverableError {
            run_dir: run_dir.to_path_buf(),
            detail: "claimant pid must be positive".to_owned(),
        });
    }
    let requested = RunTerminalClaim {
        version: 1,
        kind,
        run_id: run_id.to_owned(),
        attempt,
        claimant_pid: pid,
    };
    let path = run_terminal_claim_path(run_dir);
    let mut discarded = 0usize;
    loop {
        match create_exclusive_claim(&path, &requested) {
            Ok(true) => return Ok(requested),
            Ok(false) => {}
            Err(err) => {
                return Err(RunTerminalClaimUnrecoverableError {
                    run_dir: run_dir.to_path_buf(),
                    detail: err.to_string(),
                });
            }
        }
        let snapshot = match read_claim_snapshot(&path) {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => continue,
            Err(err) => {
                return Err(RunTerminalClaimUnrecoverableError {
                    run_dir: run_dir.to_path_buf(),
                    detail: err.to_string(),
                });
            }
        };
        let Some(existing) = snapshot.claim.as_ref() else {
            if discarded > 0 {
                return Err(RunTerminalClaimUnrecoverableError {
                    run_dir: run_dir.to_path_buf(),
                    detail: "terminal claim remained invalid after recovery".to_owned(),
                });
            }
            discarded += 1;
            if let Some(hook) = seams.before_recovery {
                hook(TerminalClaimRecoveryReason::Invalid);
            }
            if let Some(replacement) = recover_claim(
                run_dir,
                &path,
                &snapshot,
                TerminalClaimRecoveryReason::Invalid,
                run_id,
                seams,
            )? {
                return Ok(replacement);
            }
            continue;
        };
        if run_terminal_claim_matches(existing, run_id, attempt, Some(kind)) {
            return Ok(existing.clone());
        }
        if existing.kind != RunTerminalClaimKind::Publish
            || !claimant_is_confirmed_dead(existing, &seams.process)
        {
            return Ok(existing.clone());
        }
        if let Some(hook) = seams.before_recovery {
            hook(TerminalClaimRecoveryReason::DeadPublish);
        }
        if let Some(replacement) = recover_claim(
            run_dir,
            &path,
            &snapshot,
            TerminalClaimRecoveryReason::DeadPublish,
            run_id,
            seams,
        )? {
            return Ok(replacement);
        }
    }
}

struct ClaimSnapshot {
    raw: String,
    claim: Option<RunTerminalClaim>,
}

fn read_claim_snapshot(path: &Path) -> std::io::Result<Option<ClaimSnapshot>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let claim = serde_json::from_str::<RunTerminalClaim>(&raw)
        .ok()
        .filter(|c| c.version == 1 && !c.run_id.is_empty() && c.claimant_pid > 0);
    Ok(Some(ClaimSnapshot { raw, claim }))
}

fn recover_claim(
    run_dir: &Path,
    claim_path: &Path,
    inspected: &ClaimSnapshot,
    reason: TerminalClaimRecoveryReason,
    run_id: &str,
    seams: &RunTerminalClaimSeams<'_>,
) -> Result<Option<RunTerminalClaim>, RunTerminalClaimUnrecoverableError> {
    let record = create_lock_record(&format!("reflection-finalize:{run_id}"), Default::default())
        .map_err(|err| RunTerminalClaimUnrecoverableError {
        run_dir: run_dir.to_path_buf(),
        detail: err.to_string(),
    })?;
    let opts = AcquireLockOptions {
        wait_timeout_ms: Some(60_000),
        retry_delay_ms: None,
        cancellation: None,
    };
    let lock_path = run_dir.join("terminal-claim-recovery.lock");
    let res = with_lock(
        &lock_path,
        &record,
        &opts,
        || -> Result<Option<RunTerminalClaim>, String> {
            let Some(current) = read_claim_snapshot(claim_path).map_err(|e| e.to_string())? else {
                return Ok(None);
            };
            if current.raw != inspected.raw {
                return Ok(current.claim);
            }
            match reason {
                TerminalClaimRecoveryReason::Invalid => {
                    if current.claim.is_some() {
                        return Ok(current.claim);
                    }
                }
                TerminalClaimRecoveryReason::DeadPublish => {
                    let Some(ref claim) = current.claim else {
                        return Ok(None);
                    };
                    if claim.kind != RunTerminalClaimKind::Publish
                        || !claimant_is_confirmed_dead(claim, &seams.process)
                    {
                        return Ok(Some(claim.clone()));
                    }
                }
            }
            if let Err(err) = std::fs::remove_file(claim_path)
                && err.kind() != std::io::ErrorKind::NotFound
            {
                return Err(err.to_string());
            }
            Ok(None)
        },
    );
    match res {
        Ok(claim) => Ok(claim),
        Err(WithLockError::Acquire(AcquireLockError::Contention(err))) => {
            Err(RunTerminalClaimUnrecoverableError {
                run_dir: run_dir.to_path_buf(),
                detail: err.to_string(),
            })
        }
        Err(WithLockError::Acquire(err)) => Err(RunTerminalClaimUnrecoverableError {
            run_dir: run_dir.to_path_buf(),
            detail: err.to_string(),
        }),
        Err(WithLockError::User(detail)) => Err(RunTerminalClaimUnrecoverableError {
            run_dir: run_dir.to_path_buf(),
            detail,
        }),
    }
}

fn claimant_is_confirmed_dead(claim: &RunTerminalClaim, seams: &RecordedProcessSeams<'_>) -> bool {
    let liveness = match seams.pid_liveness {
        Some(probe) => probe(claim.claimant_pid),
        None => get_pid_liveness(claim.claimant_pid),
    };
    liveness == ProcessLiveness::Dead
}

fn create_exclusive_claim(path: &Path, claim: &RunTerminalClaim) -> std::io::Result<bool> {
    let candidate = path.with_file_name(format!(
        "terminal-claim.json.{}.{}.candidate",
        std::process::id(),
        random_uuid()
    ));
    let result = (|| -> std::io::Result<bool> {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&candidate)?;
        let json = serde_json::to_string_pretty(claim)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        file.write_all(format!("{json}\n").as_bytes())?;
        file.sync_all()?;
        match std::fs::hard_link(&candidate, path) {
            Ok(()) => {
                if let Some(parent) = path.parent() {
                    sync_directory(parent)?;
                }
                Ok(true)
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(err) => Err(err),
        }
    })();
    let _ = std::fs::remove_file(&candidate);
    result
}

fn sync_directory(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

fn read_json_object(path: &Path) -> std::io::Result<Option<serde_json::Map<String, Value>>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    Ok(serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|v| v.as_object().cloned()))
}
