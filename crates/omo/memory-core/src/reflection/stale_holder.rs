//! Who holds the reflection lock, decided fail-closed.
//!
//! This is the one decision behind the session's stale-lock recovery and the `omo doctor` report.
//!
//! Dead means all of: the launcher on this host is gone (or its pid now belongs to another
//! process), the publish claimant, if any, is gone, and the run recorded a valid outcome. Anything
//! that cannot be proven is "stuck", never "dead". Each rule carries a `rule:` tag named by its test.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::locks::process_identity::{
    ProcessLiveness, get_pid_liveness, get_process_start_identity, start_identities_comparable,
};
use crate::support::host::hostname;

/// Verdict for whoever holds the reflection lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReflectionLockVerdict {
    None,
    Live { run_id: String },
    Dead { run_id: String },
    Stuck { run_id: String, reason: String },
}

/// A failed read from the injectable text reader; `code` mirrors the Node error code the caller
/// distinguishes (`ENOENT` means the file is absent, anything else means it exists but is unreadable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockReadError {
    pub code: String,
}

impl LockReadError {
    pub fn new(code: impl Into<String>) -> Self {
        Self { code: code.into() }
    }
}

/// Injectable filesystem, host, PID, and process-start probes.
pub trait ReflectionLockProbe {
    fn read_text(&self, path: &Path) -> Result<String, LockReadError>;
    fn hostname(&self) -> String;
    fn pid_liveness(&self, pid: u32) -> ProcessLiveness;
    /// The recorded start identity of `pid`; `Err` means the probe itself failed.
    fn process_start_identity(&self, pid: u32) -> Result<Option<String>, String>;
}

/// The real probes: the local filesystem, `hostname()`, and the OS process identity seam.
pub struct SystemReflectionLockProbe;

impl ReflectionLockProbe for SystemReflectionLockProbe {
    fn read_text(&self, path: &Path) -> Result<String, LockReadError> {
        std::fs::read_to_string(path).map_err(|error| LockReadError::new(io_code(&error)))
    }

    fn hostname(&self) -> String {
        hostname()
    }

    fn pid_liveness(&self, pid: u32) -> ProcessLiveness {
        get_pid_liveness(pid)
    }

    fn process_start_identity(&self, pid: u32) -> Result<Option<String>, String> {
        Ok(get_process_start_identity(pid))
    }
}

fn io_code(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT".to_string(),
        std::io::ErrorKind::PermissionDenied => "EACCES".to_string(),
        other => format!("{other:?}"),
    }
}

enum JsonRead {
    Missing,
    Unreadable(String),
    Invalid,
    Ok(Value),
}

fn read_json(probe: &dyn ReflectionLockProbe, path: &Path) -> JsonRead {
    let text = match probe.read_text(path) {
        Ok(text) => text,
        Err(error) => {
            return if error.code == "ENOENT" {
                JsonRead::Missing
            } else {
                JsonRead::Unreadable(error.code)
            };
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => JsonRead::Ok(value),
        Err(_) => JsonRead::Invalid,
    }
}

fn fields_of(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

/// The shape reconciliation reads as a run outcome. A file that does not match it, or names another
/// run, is not a recorded outcome, so nothing is finalized on it.
fn is_run_outcome(value: &Value, run_id: &str) -> bool {
    let Some(fields) = fields_of(value) else {
        return false;
    };
    let Some(child_exit) = fields.get("childExit").and_then(fields_of) else {
        return false;
    };
    let version_is_one = fields
        .get("version")
        .and_then(Value::as_f64)
        .is_some_and(|version| version == 1.0);
    let names_run = fields.get("runId").and_then(Value::as_str) == Some(run_id);
    let finished = fields.get("finishedAt").is_some_and(Value::is_string);
    let attempt_ok = match fields.get("attempt") {
        None => true,
        Some(attempt) => attempt
            .as_f64()
            .is_some_and(|value| value.fract() == 0.0 && value > 0.0),
    };
    let child_exit_object = fields
        .get("childExit")
        .is_some_and(|value| value.as_object().is_some());
    let code_ok = match child_exit.get("code") {
        Some(Value::Null) => true,
        Some(Value::Number(number)) => number.as_f64().is_some(),
        _ => false,
    };
    let signal_ok = matches!(child_exit.get("signal"), Some(Value::Null) | Some(Value::String(_)));
    let timed_out_ok = fields.get("timedOut").is_some_and(Value::is_boolean);
    version_is_one
        && names_run
        && finished
        && attempt_ok
        && child_exit_object
        && code_ok
        && signal_ok
        && timed_out_ok
}

fn stuck(run_id: &str, reason: impl Into<String>) -> ReflectionLockVerdict {
    ReflectionLockVerdict::Stuck {
        run_id: run_id.to_string(),
        reason: reason.into(),
    }
}

fn pid_u32(value: f64) -> Option<u32> {
    if value >= 0.0 && value.fract() == 0.0 && value <= f64::from(u32::MAX) {
        Some(value as u32)
    } else {
        None
    }
}

fn format_pid(value: f64) -> String {
    format!("{value}")
}

/// A live pid still counts as gone when its recorded start identity proves the pid was reused. An
/// unreadable or incomparable start keeps it live.
fn launcher_liveness(
    pid: f64,
    recorded_start: Option<&str>,
    probe: &dyn ReflectionLockProbe,
) -> ProcessLiveness {
    let Some(pid_value) = pid_u32(pid) else {
        return ProcessLiveness::Unknown;
    };
    let liveness = probe.pid_liveness(pid_value);
    if liveness != ProcessLiveness::Alive {
        return liveness;
    }
    let Some(recorded) = recorded_start else {
        return liveness;
    };
    let actual = probe.process_start_identity(pid_value).unwrap_or(None);
    match actual {
        Some(actual)
            if start_identities_comparable(recorded, &actual) && recorded != actual =>
        {
            // rule:pid-reused
            ProcessLiveness::Dead
        }
        _ => ProcessLiveness::Alive,
    }
}

/// Classifies the reflection lock holder from `reflection_dir`.
pub fn classify_reflection_lock_holder(
    reflection_dir: &Path,
    probe: &dyn ReflectionLockProbe,
) -> ReflectionLockVerdict {
    let active = read_json(probe, &reflection_dir.join("active.lock"));
    let active = match active {
        JsonRead::Missing => return ReflectionLockVerdict::None,
        JsonRead::Unreadable(code) => {
            // rule:lock-unreadable
            return stuck("unknown", format!("active.lock could not be read ({code})"));
        }
        JsonRead::Invalid => {
            // rule:lock-invalid
            return stuck("unknown", "active.lock is not valid JSON");
        }
        JsonRead::Ok(value) => value,
    };
    let fields = fields_of(&active);
    let run_id = fields
        .and_then(|fields| fields.get("runId"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let launcher_pid = fields
        .and_then(|fields| fields.get("launcherPid"))
        .and_then(Value::as_f64);
    let launcher_hostname = fields
        .and_then(|fields| fields.get("launcherHostname"))
        .and_then(Value::as_str);
    let (Some(launcher_pid), Some(launcher_hostname)) = (launcher_pid, launcher_hostname) else {
        // rule:launcher-identity
        return stuck(&run_id, "its launcher identity is missing");
    };
    if launcher_hostname != probe.hostname() {
        // rule:other-host
        return stuck(
            &run_id,
            format!("it is owned by host {launcher_hostname}, which cannot be checked from here"),
        );
    }
    let recorded_start = fields
        .and_then(|fields| fields.get("launcherProcessStart"))
        .and_then(Value::as_str);
    let launcher = launcher_liveness(launcher_pid, recorded_start, probe);
    match launcher {
        // rule:launcher-alive
        ProcessLiveness::Alive => {
            return ReflectionLockVerdict::Live { run_id };
        }
        // rule:launcher-unknown
        ProcessLiveness::Unknown => {
            return stuck(
                &run_id,
                format!("launcher pid {} could not be checked", format_pid(launcher_pid)),
            );
        }
        ProcessLiveness::Dead => {}
    }
    let run_dir: PathBuf = reflection_dir.join("runs").join(&run_id);
    let outcome = read_json(probe, &run_dir.join("outcome.json"));
    let outcome = match outcome {
        // rule:outcome-missing
        JsonRead::Missing => {
            return stuck(
                &run_id,
                format!(
                    "launcher pid {} is gone and the run recorded no outcome",
                    format_pid(launcher_pid)
                ),
            );
        }
        // rule:outcome-unreadable
        JsonRead::Unreadable(code) => {
            return stuck(&run_id, format!("its outcome could not be read ({code})"));
        }
        // rule:outcome-invalid
        JsonRead::Invalid => return stuck(&run_id, "its outcome is not valid JSON"),
        JsonRead::Ok(value) => value,
    };
    if !is_run_outcome(&outcome, &run_id) {
        // rule:outcome-schema
        return stuck(&run_id, "its outcome is not a recorded run outcome");
    }
    let claim = read_json(probe, &run_dir.join("terminal-claim.json"));
    match claim {
        // rule:claim-unreadable
        JsonRead::Unreadable(code) => {
            return stuck(&run_id, format!("its terminal claim could not be read ({code})"));
        }
        // rule:claim-invalid
        JsonRead::Invalid => return stuck(&run_id, "its terminal claim is not valid JSON"),
        JsonRead::Ok(value) => {
            let claimant_pid = fields_of(&value)
                .and_then(|fields| fields.get("claimantPid"))
                .and_then(Value::as_f64);
            let Some(claimant_pid) = claimant_pid else {
                // rule:claim-no-claimant
                return stuck(&run_id, "its terminal claim names no claimant");
            };
            let claimant = match pid_u32(claimant_pid) {
                Some(pid) => probe.pid_liveness(pid),
                None => ProcessLiveness::Unknown,
            };
            match claimant {
                // rule:claimant-alive
                ProcessLiveness::Alive => return ReflectionLockVerdict::Live { run_id },
                // rule:claimant-unknown
                ProcessLiveness::Unknown => {
                    return stuck(
                        &run_id,
                        format!(
                            "publish claimant pid {} could not be checked",
                            format_pid(claimant_pid)
                        ),
                    );
                }
                ProcessLiveness::Dead => {}
            }
        }
        JsonRead::Missing => {}
    }
    ReflectionLockVerdict::Dead { run_id }
}

#[cfg(test)]
#[path = "stale_holder_tests.rs"]
mod tests;
