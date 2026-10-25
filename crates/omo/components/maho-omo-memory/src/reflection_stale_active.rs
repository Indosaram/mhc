//! Recovery of a reflection reservation whose holder died (#10377).
//!
//! Session-start reconciliation runs once per session, so a host that dies mid-publish leaves
//! runtime/reflection/active.lock held, and every later request only queues behind it. This module
//! decides, fail-closed, whether that holder is provably gone; only then is the existing
//! reconciliation asked to settle the run.

use std::path::Path;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveHolderVerdict {
    None,
    Live { run_id: String },
    Dead { run_id: String },
    Stuck { run_id: String, reason: String },
}

pub fn classify_active_reflection_holder(
    reflection_dir: &Path,
    current_hostname: Option<&str>,
) -> ActiveHolderVerdict {
    let lock_path = reflection_dir.join("active.lock");
    let content = match std::fs::read_to_string(&lock_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ActiveHolderVerdict::None,
        Err(e) => {
            return ActiveHolderVerdict::Stuck {
                run_id: "unknown".to_string(),
                reason: format!("active.lock could not be read ({e})"),
            }
        }
    };
    let value: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => {
            return ActiveHolderVerdict::Stuck {
                run_id: "unknown".to_string(),
                reason: "active.lock is not valid JSON".to_string(),
            }
        }
    };
    let run_id = value
        .get("runId")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let launcher_pid = value.get("launcherPid").and_then(Value::as_i64);
    let launcher_hostname = value.get("launcherHostname").and_then(Value::as_str);

    let (launcher_pid, launcher_hostname) = match (launcher_pid, launcher_hostname) {
        (Some(pid), Some(h)) => (pid, h),
        _ => {
            return ActiveHolderVerdict::Stuck {
                run_id,
                reason: "its launcher identity is missing".to_string(),
            }
        }
    };

    let host = current_hostname.unwrap_or("localhost");
    if launcher_hostname != host && launcher_hostname != "localhost" {
        return ActiveHolderVerdict::Stuck {
            run_id,
            reason: format!("it is owned by host {launcher_hostname}, which cannot be checked from here"),
        };
    }

    // Check if launcher_pid is alive
    #[cfg(unix)]
    let is_alive = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(launcher_pid as i32),
        None::<nix::sys::signal::Signal>,
    )
    .is_ok();
    #[cfg(not(unix))]
    let is_alive = false;

    if is_alive {
        return ActiveHolderVerdict::Live { run_id };
    }

    let run_dir = reflection_dir.join("runs").join(&run_id);
    let outcome_path = run_dir.join("outcome.json");
    let outcome_content = match std::fs::read_to_string(&outcome_path) {
        Ok(c) => c,
        Err(_) => {
            return ActiveHolderVerdict::Stuck {
                run_id,
                reason: format!("launcher pid {launcher_pid} is gone and the run recorded no outcome"),
            }
        }
    };
    let outcome: Value = match serde_json::from_str(&outcome_content) {
        Ok(v) => v,
        Err(_) => {
            return ActiveHolderVerdict::Stuck {
                run_id,
                reason: "its outcome is not valid JSON".to_string(),
            }
        }
    };
    if outcome.get("version").and_then(Value::as_u64) != Some(1) {
        return ActiveHolderVerdict::Stuck {
            run_id,
            reason: "its outcome is not a recorded run outcome".to_string(),
        };
    }

    let claim_path = run_dir.join("terminal-claim.json");
    if let Ok(claim_content) = std::fs::read_to_string(&claim_path) {
        if let Ok(claim) = serde_json::from_str::<Value>(&claim_content) {
            if let Some(claimant_pid) = claim.get("claimantPid").and_then(Value::as_i64) {
                #[cfg(unix)]
                let claimant_alive = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(claimant_pid as i32),
                    None::<nix::sys::signal::Signal>,
                )
                .is_ok();
                #[cfg(not(unix))]
                let claimant_alive = false;

                if claimant_alive {
                    return ActiveHolderVerdict::Live { run_id };
                }
            }
        }
    }

    ActiveHolderVerdict::Dead { run_id }
}

pub fn stale_reflection_lock_notice(reflection_dir: &Path, run_id: &str, reason: &str) -> String {
    format!(
        "memory reflection is stuck on a stale lock: run {run_id} still holds it and {reason}, so new reflection requests only queue. Next step: run /doctor; if no reflection run is active, move {} aside (keep a backup) and start a new session.",
        reflection_dir.join("active.lock").display()
    )
}

pub async fn recover_stale_reflection_holder<F, Fut>(
    reflection_dir: &Path,
    reconcile: F,
    should_reconcile: Option<Box<dyn Fn(&str) -> bool + Send + Sync>>,
) -> (ActiveHolderVerdict, Option<String>)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let verdict = classify_active_reflection_holder(reflection_dir, None);
    match &verdict {
        ActiveHolderVerdict::Dead { run_id } => {
            if let Some(should) = &should_reconcile {
                if !should(run_id) {
                    let notice = stale_reflection_lock_notice(
                        reflection_dir,
                        run_id,
                        "recovering it did not release the lock",
                    );
                    return (verdict, Some(notice));
                }
            }
            reconcile().await;
            let after = classify_active_reflection_holder(reflection_dir, None);
            match &after {
                ActiveHolderVerdict::Stuck { run_id: r, reason } => {
                    let notice = stale_reflection_lock_notice(reflection_dir, r, reason);
                    (after, Some(notice))
                }
                ActiveHolderVerdict::Dead { run_id: r } if r == run_id => {
                    let notice = stale_reflection_lock_notice(
                        reflection_dir,
                        r,
                        "recovering it did not release the lock",
                    );
                    (after, Some(notice))
                }
                _ => (after, None),
            }
        }
        ActiveHolderVerdict::Stuck { run_id, reason } => {
            let notice = stale_reflection_lock_notice(reflection_dir, run_id, reason);
            (verdict, Some(notice))
        }
        _ => (verdict, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_missing_active_lock_when_classified_then_verdict_none() {
        let temp = tempfile::tempdir().unwrap();
        let verdict = classify_active_reflection_holder(temp.path(), None);
        assert_eq!(verdict, ActiveHolderVerdict::None);
    }

    #[test]
    fn given_invalid_json_when_classified_then_stuck() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("active.lock"), "not-json").unwrap();
        let verdict = classify_active_reflection_holder(temp.path(), None);
        assert!(matches!(verdict, ActiveHolderVerdict::Stuck { .. }));
    }
}
