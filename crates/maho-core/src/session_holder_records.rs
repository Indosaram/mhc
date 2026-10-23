//! Port of senpi packages/coding-agent/src/core/session-holder-records.ts (senpi#3658).
//!
//! Reading the holder records of one session: which processes hold it right now. A record of a
//! dead or reused pid is reclaimed on the way, and so is one whose pid is alive but unprovable once
//! the caller has positive evidence that its host generation ended.

use std::path::Path;

use serde_json::Value;

use crate::schedule::Result;
use crate::schedule::runner_lease::{
    LockState, ProcessProbes, break_stale_lock, read_lease_text, reclaim_lock_state,
};
use crate::schedule::store::names;

/// One process holding a session (senpi `SessionHolder`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHolder {
    pub pid: u32,
    pub cwd: Option<String>,
    pub host_socket: Option<String>,
    pub host_instance: Option<String>,
}

/// The host generation a holder record names, as written by `hold_session_file`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HolderGeneration {
    pub host_instance: Option<String>,
    pub host_daemon_dir: Option<String>,
}

/// `LockProbes` plus `holder_ended`.
#[async_trait::async_trait]
pub trait SessionHolderProbes: ProcessProbes {
    /// Positive evidence that the holder's host generation has ended. Asked only about a holder whose
    /// pid is alive but whose start time cannot be compared, so a reused pid is not mistaken for it forever.
    async fn holder_ended(&self, _generation: &HolderGeneration) -> bool {
        false
    }
}

fn string_field(parsed: &Value, key: &str) -> Option<String> {
    parsed.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn holder_generation(parsed: &Value) -> HolderGeneration {
    HolderGeneration {
        host_instance: string_field(parsed, "host_instance"),
        host_daemon_dir: string_field(parsed, "host_daemon_dir"),
    }
}

/// `fence_session` advances the session file's write fence (senpi#3652). A holder released on
/// `holder_ended` may only be frozen, so it is released only after the fence moved: if it resumes, its
/// writes are refused. Without a fence, or when it cannot be written, the holder stands.
pub async fn live_holders(
    dir: &Path,
    probes: &dyn SessionHolderProbes,
    fence_session: Option<&(dyn Fn() -> std::io::Result<()> + Sync)>,
) -> Result<Vec<SessionHolder>> {
    let mut holders = Vec::new();
    for name in names(dir)? {
        if !name
            .strip_suffix(".json")
            .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()))
        {
            continue;
        }
        let path = dir.join(&name);
        let before = read_lease_text(&path)?;
        let state = reclaim_lock_state(&path, probes).await?;
        let (pid, unproven) = match state {
            LockState::Stale(raw) => {
                break_stale_lock(&path, &raw)?;
                continue;
            }
            LockState::Free | LockState::Held(None, _) => continue,
            LockState::Held(Some(holder), unproven) => (holder.pid, unproven),
        };
        let raw = read_lease_text(&path)?;
        // Never publish endpoint labels from a record replaced during the identity probe.
        let stable = raw.as_ref().filter(|raw| Some(*raw) == before.as_ref());
        let parsed: Option<Value> = stable.and_then(|raw| serde_json::from_str(raw).ok());
        if let (Some(raw), true, Some(parsed)) = (stable, unproven, parsed.as_ref())
            && probes.holder_ended(&holder_generation(parsed)).await
            && fence_session.is_some_and(|fence| fence().is_ok())
        {
            break_stale_lock(&path, raw)?;
            continue;
        }
        let host_instance = parsed
            .as_ref()
            .and_then(|parsed| string_field(parsed, "host_instance"));
        let host_socket = host_instance
            .as_ref()
            .and(parsed.as_ref())
            .and_then(|parsed| string_field(parsed, "host_socket"));
        holders.push(SessionHolder {
            pid,
            cwd: parsed
                .as_ref()
                .and_then(|parsed| string_field(parsed, "cwd")),
            host_socket,
            host_instance,
        });
    }
    Ok(holders)
}

/// Live processes that have `session_file` open right now (stale records of dead or reused pids are
/// reclaimed on the way). A caller about to start another writer uses it to wait instead of racing one.
pub async fn live_session_holders(
    session_file: &Path,
    session_id: &str,
    probes: &dyn SessionHolderProbes,
) -> Result<Vec<SessionHolder>> {
    let dir = crate::schedule::runner_lease::session_holders_dir(session_file, session_id)?;
    let file = session_file.to_string_lossy().into_owned();
    let fence = move || crate::session_write_fence::advance_session_fence(&file);
    live_holders(&dir, probes, Some(&fence)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::runner_lease::hold_session_file;
    use crate::schedule::store::RunnerIdentity;

    struct Probes {
        ended: bool,
        boot_at_ms: u64,
    }

    #[async_trait::async_trait]
    impl ProcessProbes for Probes {
        fn own_identity(&self) -> RunnerIdentity {
            RunnerIdentity {
                pid: u32::MAX,
                process_started_at_ms: 1,
            }
        }
        fn boot_at_ms(&self) -> u64 {
            self.boot_at_ms
        }
        fn now(&self) -> u64 {
            0
        }
        fn pid_alive(&self, _: u32) -> Result<bool> {
            Ok(true)
        }
        fn delivery_group_alive(&self, _: u32) -> Result<bool> {
            Ok(false)
        }
        async fn process_start_ms(&self, _: u32) -> Result<Option<u64>> {
            Ok(None)
        }
    }

    #[async_trait::async_trait]
    impl SessionHolderProbes for Probes {
        async fn holder_ended(&self, _generation: &HolderGeneration) -> bool {
            self.ended
        }
    }

    #[tokio::test]
    async fn an_unprovable_holder_stands_unless_its_generation_ended_and_the_fence_moved()
    -> Result<()> {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("session.jsonl");
        let _hold = hold_session_file(&file, "s", "/work", false)?;
        // The probes must agree with the record on the boot instant, or the record reads as stale.
        let record = crate::schedule::runner_lease::session_holders_dir(&file, "s")?
            .join(format!("{}.json", std::process::id()));
        let written: Value = serde_json::from_str(&std::fs::read_to_string(record)?)?;
        let boot_at_ms = written
            .get("bootAtMs")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let standing = live_session_holders(
            &file,
            "s",
            &Probes {
                ended: false,
                boot_at_ms,
            },
        )
        .await?;
        assert_eq!(standing.len(), 1);
        assert!(
            crate::session_write_fence::read_session_fence(&file.to_string_lossy())?.is_empty()
        );
        let reclaimed = live_session_holders(
            &file,
            "s",
            &Probes {
                ended: true,
                boot_at_ms,
            },
        )
        .await?;
        assert!(reclaimed.is_empty());
        assert!(
            !crate::session_write_fence::read_session_fence(&file.to_string_lossy())?.is_empty(),
            "the fence moves before the lease is released"
        );
        Ok(())
    }
}
