//! Who holds the reflection lock, decided fail-closed (`reflection/stale-holder.ts`, #10377).
//!
//! Dead means all of: the launcher on this host is gone (or its pid now belongs to another process),
//! the publish claimant, if any, is gone, and the run recorded a valid outcome. Anything that cannot
//! be proven is "stuck", never "dead".

use std::path::Path;

use crate::locks::process_identity::{
    ProcessLiveness, get_pid_liveness, get_process_start_identity, start_identities_comparable,
};
use crate::support::host::hostname as read_hostname;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReflectionLockVerdict {
    None,
    Live { run_id: String },
    Dead { run_id: String },
    Stuck { run_id: String, reason: String },
}

impl ReflectionLockVerdict {
    pub fn run_id(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Live { run_id } | Self::Dead { run_id } | Self::Stuck { run_id, .. } => {
                Some(run_id)
            }
        }
    }
}

/// `read_text` reports the filesystem error code as its message, so a missing lock is distinguishable.
pub type ReflectionLockReadText<'a> = dyn Fn(&Path) -> Result<String, String> + 'a;

/// Test seams: production reads the filesystem and the real process table.
#[derive(Default, Clone, Copy)]
pub struct ReflectionLockProbe<'a> {
    pub read_text: Option<&'a ReflectionLockReadText<'a>>,
    pub hostname: Option<&'a dyn Fn() -> String>,
    pub pid_liveness: Option<&'a dyn Fn(u32) -> ProcessLiveness>,
    pub process_start: Option<&'a dyn Fn(u32) -> Option<String>>,
}

enum JsonRead {
    Missing,
    Unreadable(String),
    Invalid,
    Ok(serde_json::Value),
}

fn read_json(path: &Path, read_text: &ReflectionLockReadText<'_>) -> JsonRead {
    let text = match read_text(path) {
        Ok(text) => text,
        Err(code) if code == "ENOENT" || code == "NotFound" => return JsonRead::Missing,
        Err(code) => return JsonRead::Unreadable(code),
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(value) => JsonRead::Ok(value),
        Err(_) => JsonRead::Invalid,
    }
}

fn default_read_text(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT".to_string(),
        _ => error.to_string(),
    })
}

fn object_of(value: &serde_json::Value) -> Option<&serde_json::Map<String, serde_json::Value>> {
    value.as_object()
}

/// The shape reconciliation reads as a `RunOutcome`. A file that does not match it, or names another
/// run, is not a recorded outcome, so nothing is finalized on it.
fn is_run_outcome(value: &serde_json::Value, run_id: &str) -> bool {
    let Some(fields) = object_of(value) else {
        return false;
    };
    let child_exit = fields.get("childExit").and_then(object_of);
    let integer_attempt = match fields.get("attempt") {
        None => true,
        Some(attempt) => attempt
            .as_f64()
            .is_some_and(|attempt| attempt.fract() == 0.0 && attempt > 0.0),
    };
    let code_ok = match child_exit.and_then(|exit| exit.get("code")) {
        None | Some(serde_json::Value::Null) => true,
        Some(code) => code.is_number(),
    };
    let signal_ok = match child_exit.and_then(|exit| exit.get("signal")) {
        None | Some(serde_json::Value::Null) => true,
        Some(signal) => signal.is_string(),
    };
    fields.get("version").and_then(serde_json::Value::as_f64) == Some(1.0)
        && fields.get("runId").and_then(serde_json::Value::as_str) == Some(run_id)
        && fields
            .get("finishedAt")
            .is_some_and(serde_json::Value::is_string)
        && integer_attempt
        && fields.get("childExit").is_some_and(|exit| exit.is_object())
        && code_ok
        && signal_ok
        && fields
            .get("timedOut")
            .is_some_and(serde_json::Value::is_boolean)
}

fn stuck(run_id: &str, reason: impl Into<String>) -> ReflectionLockVerdict {
    ReflectionLockVerdict::Stuck {
        run_id: run_id.to_string(),
        reason: reason.into(),
    }
}

/// A live pid still counts as gone when its recorded start identity proves the pid was reused; an
/// unreadable or incomparable start keeps it live (`rule:pid-reused`).
fn launcher_liveness(
    pid: u32,
    recorded_start: Option<&serde_json::Value>,
    probe: &ReflectionLockProbe<'_>,
) -> ProcessLiveness {
    let liveness = probe
        .pid_liveness
        .map_or_else(|| get_pid_liveness(pid), |check| check(pid));
    if liveness != ProcessLiveness::Alive {
        return liveness;
    }
    let Some(recorded_start) = recorded_start.and_then(serde_json::Value::as_str) else {
        return liveness;
    };
    let actual = probe
        .process_start
        .map_or_else(|| get_process_start_identity(pid), |start| start(pid));
    if let Some(actual) = actual
        && start_identities_comparable(recorded_start, &actual)
        && recorded_start != actual
    {
        return ProcessLiveness::Dead;
    }
    ProcessLiveness::Alive
}

/// Classifies the holder of `<reflection>/active.lock`, fail-closed.
pub fn classify_reflection_lock_holder(
    reflection_dir: &Path,
    probe: &ReflectionLockProbe<'_>,
) -> ReflectionLockVerdict {
    let default_read: &ReflectionLockReadText<'_> = &default_read_text;
    let read_text = probe.read_text.unwrap_or(default_read);
    let active = read_json(&reflection_dir.join("active.lock"), read_text);
    let value = match active {
        JsonRead::Missing => return ReflectionLockVerdict::None,
        // rule:lock-unreadable
        JsonRead::Unreadable(code) => {
            return stuck("unknown", format!("active.lock could not be read ({code})"));
        }
        // rule:lock-invalid
        JsonRead::Invalid => return stuck("unknown", "active.lock is not valid JSON"),
        JsonRead::Ok(value) => value,
    };
    let Some(fields) = object_of(&value) else {
        return stuck("unknown", "active.lock is not a JSON object");
    };
    let run_id = fields
        .get("runId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let Some(launcher_pid) = fields
        .get("launcherPid")
        .and_then(serde_json::Value::as_f64)
        .filter(|pid| *pid >= 0.0)
    else {
        // rule:launcher-identity
        return stuck(&run_id, "its launcher identity is missing");
    };
    let Some(launcher_hostname) = fields
        .get("launcherHostname")
        .and_then(serde_json::Value::as_str)
    else {
        return stuck(&run_id, "its launcher identity is missing");
    };
    let Ok(launcher_pid) = u32::try_from(launcher_pid as u64) else {
        return stuck(&run_id, "its launcher identity is missing");
    };
    let local_hostname = probe
        .hostname
        .map_or_else(read_hostname, |hostname| hostname());
    if launcher_hostname != local_hostname {
        // rule:other-host
        return stuck(
            &run_id,
            format!("it is owned by host {launcher_hostname}, which cannot be checked from here"),
        );
    }
    match launcher_liveness(launcher_pid, fields.get("launcherProcessStart"), probe) {
        // rule:launcher-alive
        ProcessLiveness::Alive => return ReflectionLockVerdict::Live { run_id },
        // rule:launcher-unknown
        ProcessLiveness::Unknown => {
            return stuck(
                &run_id,
                format!("launcher pid {launcher_pid} could not be checked"),
            );
        }
        ProcessLiveness::Dead => {}
    }
    let run_dir = reflection_dir.join("runs").join(&run_id);
    let outcome = read_json(&run_dir.join("outcome.json"), read_text);
    match outcome {
        // rule:outcome-missing
        JsonRead::Missing => {
            return stuck(
                &run_id,
                format!("launcher pid {launcher_pid} is gone and the run recorded no outcome"),
            );
        }
        // rule:outcome-unreadable
        JsonRead::Unreadable(code) => {
            return stuck(&run_id, format!("its outcome could not be read ({code})"));
        }
        // rule:outcome-invalid
        JsonRead::Invalid => return stuck(&run_id, "its outcome is not valid JSON"),
        // rule:outcome-schema
        JsonRead::Ok(value) if !is_run_outcome(&value, &run_id) => {
            return stuck(&run_id, "its outcome is not a recorded run outcome");
        }
        JsonRead::Ok(_) => {}
    }
    let claim = read_json(&run_dir.join("terminal-claim.json"), read_text);
    match claim {
        // rule:claim-unreadable
        JsonRead::Unreadable(code) => {
            return stuck(
                &run_id,
                format!("its terminal claim could not be read ({code})"),
            );
        }
        // rule:claim-invalid
        JsonRead::Invalid => return stuck(&run_id, "its terminal claim is not valid JSON"),
        JsonRead::Ok(value) => {
            let claimant = object_of(&value)
                .and_then(|fields| fields.get("claimantPid"))
                .and_then(serde_json::Value::as_f64)
                .filter(|pid| *pid >= 0.0);
            // rule:claim-no-claimant
            let Some(claimant) = claimant.and_then(|pid| u32::try_from(pid as u64).ok()) else {
                return stuck(&run_id, "its terminal claim names no claimant");
            };
            let liveness = probe
                .pid_liveness
                .map_or_else(|| get_pid_liveness(claimant), |check| check(claimant));
            match liveness {
                // rule:claimant-alive
                ProcessLiveness::Alive => return ReflectionLockVerdict::Live { run_id },
                // rule:claimant-unknown
                ProcessLiveness::Unknown => {
                    return stuck(
                        &run_id,
                        format!("publish claimant pid {claimant} could not be checked"),
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
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const OUTCOME: &str = r#"{"version":1,"runId":"run-1","attempt":1,"finishedAt":"2026-01-01T00:00:00.000Z","childExit":{"code":0,"signal":null},"timedOut":false}"#;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn lock(root: &Path, body: serde_json::Value) {
        write(root, "active.lock", &body.to_string());
    }

    fn alive(pid: u32) -> ProcessLiveness {
        if pid == 4242 {
            ProcessLiveness::Alive
        } else {
            ProcessLiveness::Dead
        }
    }

    fn probe_for<'a>(
        hostname: &'a dyn Fn() -> String,
        liveness: &'a dyn Fn(u32) -> ProcessLiveness,
    ) -> ReflectionLockProbe<'a> {
        ReflectionLockProbe {
            read_text: None,
            hostname: Some(hostname),
            pid_liveness: Some(liveness),
            process_start: None,
        }
    }

    fn fixture() -> BTreeMap<&'static str, String> {
        BTreeMap::from([
            (
                "lock",
                serde_json::json!({"runId":"run-1","launcherPid":4242,"launcherHostname":"host"})
                    .to_string(),
            ),
            ("outcome", OUTCOME.to_string()),
        ])
    }

    #[test]
    fn no_lock_means_no_holder() {
        let root = dir();
        let host = || "host".to_string();
        let live = |pid: u32| alive(pid);
        assert_eq!(
            classify_reflection_lock_holder(root.path(), &probe_for(&host, &live)),
            ReflectionLockVerdict::None
        );
    }

    #[test]
    fn a_live_launcher_owns_the_lock_and_a_dead_one_with_an_outcome_is_dead() {
        let root = dir();
        let fixture = fixture();
        lock(root.path(), serde_json::from_str(&fixture["lock"]).unwrap());
        let host = || "host".to_string();
        let live = |pid: u32| alive(pid);
        assert_eq!(
            classify_reflection_lock_holder(root.path(), &probe_for(&host, &live)),
            ReflectionLockVerdict::Live {
                run_id: "run-1".into()
            }
        );
        write(root.path(), "runs/run-1/outcome.json", &fixture["outcome"]);
        let dead = |_: u32| ProcessLiveness::Dead;
        assert_eq!(
            classify_reflection_lock_holder(root.path(), &probe_for(&host, &dead)),
            ReflectionLockVerdict::Dead {
                run_id: "run-1".into()
            }
        );
    }

    #[test]
    fn a_dead_launcher_without_a_recorded_outcome_is_stuck() {
        let root = dir();
        lock(
            root.path(),
            serde_json::json!({"runId":"run-1","launcherPid":4242,"launcherHostname":"host"}),
        );
        let host = || "host".to_string();
        let dead = |_: u32| ProcessLiveness::Dead;
        let ReflectionLockVerdict::Stuck { reason, .. } =
            classify_reflection_lock_holder(root.path(), &probe_for(&host, &dead))
        else {
            panic!("a missing outcome is never provably dead");
        };
        assert_eq!(
            reason,
            "launcher pid 4242 is gone and the run recorded no outcome"
        );
    }

    #[test]
    fn unknown_liveness_another_host_and_an_unreadable_lock_are_all_stuck() {
        let root = dir();
        let host = || "host".to_string();
        let unknown = |_: u32| ProcessLiveness::Unknown;
        lock(
            root.path(),
            serde_json::json!({"runId":"run-1","launcherPid":4242,"launcherHostname":"other"}),
        );
        assert!(matches!(
            classify_reflection_lock_holder(root.path(), &probe_for(&host, &unknown)),
            ReflectionLockVerdict::Stuck { .. }
        ));
        lock(
            root.path(),
            serde_json::json!({"runId":"run-1","launcherPid":4242,"launcherHostname":"host"}),
        );
        let ReflectionLockVerdict::Stuck { reason, .. } =
            classify_reflection_lock_holder(root.path(), &probe_for(&host, &unknown))
        else {
            panic!("unknown liveness is never dead");
        };
        assert_eq!(reason, "launcher pid 4242 could not be checked");
        // An unreadable lock is reported, never treated as absent.
        let denied = |_: &Path| Err("EACCES".to_string());
        let probe = ReflectionLockProbe {
            read_text: Some(&denied),
            ..Default::default()
        };
        assert!(matches!(
            classify_reflection_lock_holder(root.path(), &probe),
            ReflectionLockVerdict::Stuck { .. }
        ));
    }

    #[test]
    fn a_live_publish_claimant_keeps_the_run_alive_and_a_dead_one_lets_it_settle() {
        let root = dir();
        write(
            root.path(),
            "active.lock",
            &serde_json::json!({"runId":"run-1","launcherPid":4242,"launcherHostname":"host"})
                .to_string(),
        );
        write(root.path(), "runs/run-1/outcome.json", OUTCOME);
        write(
            root.path(),
            "runs/run-1/terminal-claim.json",
            &serde_json::json!({"version":1,"runId":"run-1","attempt":1,"kind":"publish","claimantPid":777}).to_string(),
        );
        let host = || "host".to_string();
        let claimant_alive = |pid: u32| {
            if pid == 777 {
                ProcessLiveness::Alive
            } else {
                ProcessLiveness::Dead
            }
        };
        assert_eq!(
            classify_reflection_lock_holder(root.path(), &probe_for(&host, &claimant_alive)),
            ReflectionLockVerdict::Live {
                run_id: "run-1".into()
            }
        );
        let all_dead = |_: u32| ProcessLiveness::Dead;
        assert_eq!(
            classify_reflection_lock_holder(root.path(), &probe_for(&host, &all_dead)),
            ReflectionLockVerdict::Dead {
                run_id: "run-1".into()
            }
        );
    }
}
