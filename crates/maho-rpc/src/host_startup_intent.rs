//! The startup intent of one endpoint (senpi#4602, A-amended section 5): recovery evidence a
//! launcher writes under the endpoint's ensure lock BEFORE it spawns, so a later lock holder can
//! tell a start that may still be running from one that is provably over. It is not a progress
//! channel and grants no right to signal anything.
//!
//! The next holder reconciles a leftover intent through the candidate's OWN identity: the child the
//! launcher bound, or else the supervisor that recorded the same attempt nonce in its generation
//! directory. Only a candidate proven gone is cleared at once. A candidate nobody can identify is
//! UNKNOWN, and stays refused until the intent is older than the settled-result bound - the longest
//! any start, cleanup included, runs.

use crate::host_daemon_paths::{HostDaemonPaths, HostDaemonStateError, generation_paths};
use crate::host_daemon_state::{parse_json, read_file_or_undefined, write_state_file};
use crate::host_decision::{HostEnsureRefusedError, HostRefusalReason};
use crate::host_ensure_stop::{DaemonPidFile, PidFileOwnership, resolve_pid_file_ownership};
use crate::host_reservations::read_process_start_time;
use crate::host_stalled_evidence::{child_stalled_stop_max_ms, now_ms};
use crate::host_startup_policy::startup_envelope;
use crate::host_startup_wire::STARTUP_ATTEMPT_FILE;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const STARTUP_INTENT_FILE: &str = "startup-intent.json";
pub const STARTUP_INTENT_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupIntent {
    pub attempt: String,
    pub instance_id: String,
    pub generation: u64,
    pub launcher: DaemonPidFile,
    /// Absent until the launcher bound the spawned child's identity.
    pub child: Option<DaemonPidFile>,
    /// Wall-clock milliseconds: the intent outlives the process that wrote it, so no monotonic origin applies.
    pub written_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupIntentRecord {
    Valid(StartupIntent),
    Unreadable { written_at: i64 },
}

pub fn startup_intent_path(daemon_dir: &Path) -> PathBuf {
    daemon_dir.join(STARTUP_INTENT_FILE)
}

fn pid_file_json(pid_file: &DaemonPidFile) -> Value {
    json!({
        "pid": pid_file.pid,
        "processStartTime": pid_file.process_start_time,
    })
}

/// Written (mode 0600, by rename) under the ensure lock, before the spawn.
pub fn write_startup_intent(
    daemon_dir: &Path,
    intent: &StartupIntent,
) -> Result<(), HostDaemonStateError> {
    let path = startup_intent_path(daemon_dir);
    let temp = daemon_dir.join(format!(
        "{STARTUP_INTENT_FILE}.{}.{}.tmp",
        std::process::id(),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let mut payload = json!({
        "version": STARTUP_INTENT_VERSION,
        "attempt": intent.attempt,
        "instanceId": intent.instance_id,
        "generation": intent.generation,
        "launcher": pid_file_json(&intent.launcher),
        "writtenAt": intent.written_at,
    });
    if let Some(child) = &intent.child {
        payload["child"] = pid_file_json(child);
    }
    write_state_file(&temp, &payload)?;
    std::fs::rename(&temp, &path).map_err(|source| {
        let _ = std::fs::remove_file(&temp);
        HostDaemonStateError { path, source }
    })
}

fn pid_file_of(value: Option<&Value>) -> Option<Option<DaemonPidFile>> {
    let value = value?;
    let obj = value.as_object()?;
    let pid = u32::try_from(obj.get("pid")?.as_u64()?).ok()?;
    if pid == 0 {
        return Some(None);
    }
    let process_start_time = match obj.get("processStartTime") {
        Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        _ => return Some(None),
    };
    Some(Some(DaemonPidFile {
        pid,
        process_start_time,
    }))
}

fn parse_intent_record(record: Option<&Map<String, Value>>, modified: i64) -> StartupIntentRecord {
    let unreadable = StartupIntentRecord::Unreadable {
        written_at: modified,
    };
    let Some(record) = record else {
        return unreadable;
    };
    if record.get("version").and_then(Value::as_u64) != Some(STARTUP_INTENT_VERSION) {
        return unreadable;
    }
    let Some(attempt) = record.get("attempt").and_then(Value::as_str) else {
        return unreadable;
    };
    let Some(instance_id) = record.get("instanceId").and_then(Value::as_str) else {
        return unreadable;
    };
    let Some(generation) = record.get("generation").and_then(Value::as_u64) else {
        return unreadable;
    };
    let Some(written_at) = record.get("writtenAt").and_then(Value::as_i64) else {
        return unreadable;
    };
    let Some(Some(launcher)) = pid_file_of(record.get("launcher")) else {
        return unreadable;
    };
    let child = match record.get("child") {
        None => None,
        Some(val) => match pid_file_of(Some(val)) {
            Some(Some(parsed)) => Some(parsed),
            _ => return unreadable,
        },
    };
    StartupIntentRecord::Valid(StartupIntent {
        attempt: attempt.to_owned(),
        instance_id: instance_id.to_owned(),
        generation,
        launcher,
        child,
        written_at,
    })
}

/// `Unreadable` carries the file's age source, so even a torn intent expires.
pub fn read_startup_intent(daemon_dir: &Path) -> Option<StartupIntentRecord> {
    let path = startup_intent_path(daemon_dir);
    let meta = std::fs::metadata(&path).ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64);
    let text = read_file_or_undefined(&path).ok().flatten();
    let parsed = parse_json(text.as_deref());
    Some(parse_intent_record(parsed.as_ref(), modified))
}

pub fn clear_startup_intent(daemon_dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(startup_intent_path(daemon_dir)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// The identity a supervisor recorded for `attempt` in its own generation directory, if it got that far.
pub fn read_startup_attempt_record(generation_dir: &Path, attempt: &str) -> Option<DaemonPidFile> {
    let text = read_file_or_undefined(&generation_dir.join(STARTUP_ATTEMPT_FILE))
        .ok()
        .flatten();
    let record = parse_json(text.as_deref())?;
    if record.get("attempt").and_then(Value::as_str) != Some(attempt) {
        return None;
    }
    let pid = u32::try_from(record.get("pid")?.as_u64()?).ok()?;
    if pid == 0 {
        return None;
    }
    let process_start_time = match record.get("processStartTime") {
        Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        _ => return None,
    };
    Some(DaemonPidFile {
        pid,
        process_start_time,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupIntentVerdict {
    None,
    CandidateGone,
    Expired,
    CandidateLive,
    Unknown,
}

pub fn reconcile_startup_intent(
    intent: Option<&StartupIntentRecord>,
    matches: impl Fn(&DaemonPidFile) -> Option<bool>,
    attempt_record: impl Fn(&str, &str) -> Option<DaemonPidFile>,
    expiry_ms: i64,
    now: i64,
) -> StartupIntentVerdict {
    let Some(intent) = intent else {
        return StartupIntentVerdict::None;
    };
    let written_at = match intent {
        StartupIntentRecord::Unreadable { written_at } => *written_at,
        StartupIntentRecord::Valid(valid) => valid.written_at,
    };
    let expired_or = |verdict: StartupIntentVerdict| {
        if now - written_at >= expiry_ms {
            StartupIntentVerdict::Expired
        } else {
            verdict
        }
    };
    let StartupIntentRecord::Valid(valid) = intent else {
        return expired_or(StartupIntentVerdict::Unknown);
    };
    let owned_candidate;
    let candidate = match valid.child.as_ref() {
        Some(child) => child,
        None => {
            let Some(found) = attempt_record(&valid.instance_id, &valid.attempt) else {
                return expired_or(StartupIntentVerdict::Unknown);
            };
            owned_candidate = found;
            &owned_candidate
        }
    };
    match matches(candidate) {
        None => expired_or(StartupIntentVerdict::Unknown),
        Some(true) => StartupIntentVerdict::CandidateLive,
        Some(false) => StartupIntentVerdict::CandidateGone,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupIntentRefusalReason {
    HostStalled,
    EndpointOccupied,
}

impl StartupIntentRefusalReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HostStalled => "host_stalled",
            Self::EndpointOccupied => "endpoint_occupied",
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("RPC socket {socket} refused: {} - {detail}", reason.as_str())]
pub struct StartupIntentRefusal {
    pub socket: String,
    pub reason: StartupIntentRefusalReason,
    pub detail: String,
}

impl StartupIntentRefusal {
    pub fn to_ensure_refused_error(&self) -> Option<HostEnsureRefusedError> {
        match self.reason {
            StartupIntentRefusalReason::HostStalled => Some(HostEnsureRefusedError::new(
                &self.socket,
                HostRefusalReason::HostStalled,
                None,
            )),
            StartupIntentRefusalReason::EndpointOccupied => None,
        }
    }
}

/// Run under the ensure lock before a start: a leftover intent whose candidate is gone, or that
/// expired, is cleared; one whose candidate may still run refuses this start and stays as evidence.
/// Nothing is signalled.
pub fn settle_startup_intent(
    paths: &HostDaemonPaths,
    socket: &str,
    read_start_time: Option<&(dyn Fn(u32) -> Option<String> + Sync)>,
) -> Result<(), StartupIntentRefusal> {
    let env: HashMap<String, String> = std::env::vars().collect();
    let expiry_ms = startup_envelope(Some(child_stalled_stop_max_ms(&env))).settled as i64;
    let intent = read_startup_intent(&paths.dir);
    let read_fn = |pid: u32| -> Option<String> {
        match read_start_time {
            Some(f) => f(pid),
            None => read_process_start_time(pid),
        }
    };
    let verdict = reconcile_startup_intent(
        intent.as_ref(),
        |record| match resolve_pid_file_ownership(record, read_fn) {
            PidFileOwnership::Owns => Some(true),
            PidFileOwnership::Gone => Some(false),
            PidFileOwnership::Unknown => None,
        },
        |instance_id, attempt| {
            read_startup_attempt_record(&generation_paths(paths, instance_id).dir, attempt)
        },
        expiry_ms,
        now_ms(),
    );
    match verdict {
        StartupIntentVerdict::None => Ok(()),
        StartupIntentVerdict::CandidateGone | StartupIntentVerdict::Expired => {
            let _ = clear_startup_intent(&paths.dir);
            Ok(())
        }
        StartupIntentVerdict::CandidateLive => Err(StartupIntentRefusal {
            socket: socket.to_owned(),
            reason: StartupIntentRefusalReason::HostStalled,
            detail: format!(
                "a previous start of this endpoint is still running and never registered ({})",
                startup_intent_path(&paths.dir).display()
            ),
        }),
        StartupIntentVerdict::Unknown => Err(StartupIntentRefusal {
            socket: socket.to_owned(),
            reason: StartupIntentRefusalReason::EndpointOccupied,
            detail: format!(
                "a previous start of this endpoint cannot be identified; {} is kept until its candidate is proven gone or {expiry_ms}ms after it was written",
                startup_intent_path(&paths.dir).display()
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_intent() -> StartupIntent {
        StartupIntent {
            attempt: "att-1".into(),
            instance_id: "gen-1".into(),
            generation: 1,
            launcher: DaemonPidFile {
                pid: 100,
                process_start_time: Some("1000".into()),
            },
            child: Some(DaemonPidFile {
                pid: 200,
                process_start_time: Some("2000".into()),
            }),
            written_at: 5_000,
        }
    }

    #[test]
    fn startup_intent_round_trips_and_unreadable_marks_age() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_startup_intent(temp.path()), None);
        let intent = sample_intent();
        write_startup_intent(temp.path(), &intent).expect("write");
        assert_eq!(
            read_startup_intent(temp.path()),
            Some(StartupIntentRecord::Valid(intent))
        );
        clear_startup_intent(temp.path()).expect("clear");
        assert_eq!(read_startup_intent(temp.path()), None);

        std::fs::write(startup_intent_path(temp.path()), "{bad").expect("write bad");
        assert!(matches!(
            read_startup_intent(temp.path()),
            Some(StartupIntentRecord::Unreadable { .. })
        ));
    }

    #[test]
    fn reconcile_handles_live_gone_unknown_and_expired_candidates() {
        let intent = StartupIntentRecord::Valid(sample_intent());
        assert_eq!(
            reconcile_startup_intent(Some(&intent), |_| Some(true), |_, _| None, 1000, 5500),
            StartupIntentVerdict::CandidateLive
        );
        assert_eq!(
            reconcile_startup_intent(Some(&intent), |_| Some(false), |_, _| None, 1000, 5500),
            StartupIntentVerdict::CandidateGone
        );
        assert_eq!(
            reconcile_startup_intent(Some(&intent), |_| None, |_, _| None, 1000, 5500),
            StartupIntentVerdict::Unknown
        );
        assert_eq!(
            reconcile_startup_intent(Some(&intent), |_| None, |_, _| None, 1000, 6000),
            StartupIntentVerdict::Expired
        );
    }

    #[test]
    fn settle_clears_gone_intent_and_refuses_live_intent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = crate::host_daemon_paths::host_daemon_directory_paths(temp.path());
        let own = std::process::id();
        let own_start = read_process_start_time(own);
        let live_intent = StartupIntent {
            attempt: "att-live".into(),
            instance_id: "gen-1".into(),
            generation: 1,
            launcher: DaemonPidFile {
                pid: own,
                process_start_time: own_start.clone(),
            },
            child: Some(DaemonPidFile {
                pid: own,
                process_start_time: own_start,
            }),
            written_at: now_ms(),
        };
        write_startup_intent(&paths.dir, &live_intent).expect("write");
        let refusal = settle_startup_intent(&paths, "/tmp/rpc.sock", None).unwrap_err();
        assert_eq!(refusal.reason, StartupIntentRefusalReason::HostStalled);
        assert!(startup_intent_path(&paths.dir).exists());

        let gone_intent = StartupIntent {
            child: Some(DaemonPidFile {
                pid: own,
                process_start_time: Some("wrong-start".into()),
            }),
            ..live_intent
        };
        write_startup_intent(&paths.dir, &gone_intent).expect("write gone");
        settle_startup_intent(&paths, "/tmp/rpc.sock", None).expect("cleared");
        assert!(!startup_intent_path(&paths.dir).exists());
    }
}
