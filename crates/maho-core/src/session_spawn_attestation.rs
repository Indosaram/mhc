//! Port of senpi packages/coding-agent/src/core/session-spawn-attestation.ts.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use crate::config::get_agent_dir;
use crate::kernel_process_identity::{KernelProcessIdentity, read_kernel_process_identity};

pub const SESSION_SPAWNS_DIR: &str = "session-spawns";
const RECORD_VERSION: u32 = 1;
const MAX_ANCESTRY_DEPTH: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSpawnRecord {
    pub version: u32,
    pub pid: u32,
    pub started_at_ms: u64,
    pub boot_id: Option<String>,
    pub session_id: String,
    pub host_pid: u32,
    pub host_started_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PidSessionReason {
    #[serde(rename = "process_not_found")]
    ProcessNotFound,
    #[serde(rename = "no_attested_ancestor")]
    NoAttestedAncestor,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PidSession {
    Attested {
        pid: u32,
        session_id: String,
        attested_pid: u32,
        host_pid: u32,
        chain: Vec<u32>,
    },
    Unattested {
        pid: u32,
        reason: PidSessionReason,
    },
}

pub struct SessionSpawnAttestation {
    written: tokio::sync::oneshot::Receiver<Option<SessionSpawnRecord>>,
    shared: Arc<OnceCell<Option<SessionSpawnRecord>>>,
    path: PathBuf,
}

impl SessionSpawnAttestation {
    pub async fn recorded(&mut self) -> bool {
        self.resolve().await.is_some()
    }

    async fn resolve(&mut self) -> Option<SessionSpawnRecord> {
        if let Some(existing) = self.shared.get() {
            return existing.clone();
        }
        let record: Option<SessionSpawnRecord> = (&mut self.written).await.unwrap_or_default();
        let _ = self.shared.set(record.clone());
        record
    }

    pub async fn release(mut self) {
        let Some(mine) = self.resolve().await else {
            return;
        };
        if let Some(current) = read_record(&self.path).await
            && same_record(&current, &mine)
        {
            let _ = tokio::fs::remove_file(&self.path).await;
        }
    }
}

pub fn session_spawns_dir(agent_dir: Option<&str>) -> PathBuf {
    let base = agent_dir.map_or_else(|| PathBuf::from(get_agent_dir()), PathBuf::from);
    base.join(SESSION_SPAWNS_DIR)
}

static OWN_IDENTITY: OnceCell<Option<KernelProcessIdentity>> = OnceCell::const_new();

async fn get_own_identity() -> Option<KernelProcessIdentity> {
    OWN_IDENTITY
        .get_or_init(|| read_kernel_process_identity(std::process::id()))
        .await
        .clone()
}

/// Records that `pid`, a child this process just spawned, runs for `session_id`. Never fails fatally.
pub fn attest_session_spawn(
    session_id: &str,
    pid: u32,
    dir: Option<&Path>,
) -> SessionSpawnAttestation {
    let spawns_dir = dir.map_or_else(|| session_spawns_dir(None), PathBuf::from);
    let path = spawns_dir.join(format!("{pid}.json"));
    let path_for_task = path.clone();
    let session_id = session_id.to_owned();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let shared = Arc::new(OnceCell::new());
    let shared_for_task = Arc::clone(&shared);
    tokio::spawn(async move {
        let child = read_kernel_process_identity(pid).await;
        let record = write_record(&spawns_dir, &path_for_task, &session_id, pid, child).await;
        let _ = shared_for_task.set(record.clone());
        let _ = tx.send(record);
    });
    SessionSpawnAttestation {
        written: rx,
        shared,
        path,
    }
}

async fn write_record(
    dir: &Path,
    path: &Path,
    session_id: &str,
    pid: u32,
    child: Option<KernelProcessIdentity>,
) -> Option<SessionSpawnRecord> {
    let host = get_own_identity().await?;
    let child = child?;
    let host_pid = std::process::id();
    if child.ppid != host_pid {
        return None;
    }
    let record = SessionSpawnRecord {
        version: RECORD_VERSION,
        pid,
        started_at_ms: child.started_at_ms,
        boot_id: child.boot_id.clone(),
        session_id: session_id.to_owned(),
        host_pid,
        host_started_at_ms: host.started_at_ms,
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(dir).ok()?;
    }
    #[cfg(not(unix))]
    {
        tokio::fs::create_dir_all(dir).await.ok()?;
    }

    let temp = PathBuf::from(format!("{}.{}.tmp", path.display(), host_pid));
    let payload = serde_json::to_vec(&record).ok()?;
    {
        use tokio::io::AsyncWriteExt;
        let mut opts = tokio::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut file = opts.open(&temp).await.ok()?;
        file.write_all(&payload).await.ok()?;
    }

    let result = match tokio::fs::hard_link(&temp, path).await {
        Ok(()) => Some(record),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            if let Some(existing) = read_record(path).await
                && record_holds(&existing, pid, &child).await
            {
                if same_record(&existing, &record) {
                    Some(record)
                } else {
                    None
                }
            } else if tokio::fs::rename(&temp, path).await.is_ok() {
                Some(record)
            } else {
                None
            }
        }
        Err(_) => None,
    };
    let _ = tokio::fs::remove_file(&temp).await;
    result
}

fn same_record(a: &SessionSpawnRecord, b: &SessionSpawnRecord) -> bool {
    a.pid == b.pid
        && a.started_at_ms == b.started_at_ms
        && a.boot_id == b.boot_id
        && a.session_id == b.session_id
        && a.host_pid == b.host_pid
        && a.host_started_at_ms == b.host_started_at_ms
}

/// Which session owns `pid`, from the kernel's ancestry and the spawners' records.
pub async fn resolve_pid_session(pid: u32, dir: Option<&Path>) -> PidSession {
    let spawns_dir = dir.map_or_else(|| session_spawns_dir(None), PathBuf::from);
    let mut chain = Vec::new();
    let mut current = pid;
    let Some(mut identity) = read_kernel_process_identity(current).await else {
        return PidSession::Unattested {
            pid,
            reason: PidSessionReason::ProcessNotFound,
        };
    };
    if identity.zombie {
        return PidSession::Unattested {
            pid,
            reason: PidSessionReason::ProcessNotFound,
        };
    }
    while chain.len() < MAX_ANCESTRY_DEPTH {
        chain.push(current);
        let record_path = spawns_dir.join(format!("{current}.json"));
        if let Some(record) = read_record(&record_path).await
            && record_holds(&record, current, &identity).await
        {
            return PidSession::Attested {
                pid,
                session_id: record.session_id,
                attested_pid: current,
                host_pid: record.host_pid,
                chain,
            };
        }
        if identity.ppid <= 1 {
            break;
        }
        current = identity.ppid;
        let Some(parent_id) = read_kernel_process_identity(current).await else {
            break;
        };
        identity = parent_id;
    }
    PidSession::Unattested {
        pid,
        reason: PidSessionReason::NoAttestedAncestor,
    }
}

async fn record_holds(
    record: &SessionSpawnRecord,
    pid: u32,
    identity: &KernelProcessIdentity,
) -> bool {
    if record.pid != pid
        || record.started_at_ms != identity.started_at_ms
        || record.boot_id != identity.boot_id
        || record.host_pid != identity.ppid
    {
        return false;
    }
    let Some(host) = read_kernel_process_identity(record.host_pid).await else {
        return false;
    };
    !host.zombie && host.started_at_ms == record.host_started_at_ms
}

async fn read_record(path: &Path) -> Option<SessionSpawnRecord> {
    let raw = tokio::fs::read_to_string(path).await.ok()?;
    let record: SessionSpawnRecord = serde_json::from_str(&raw).ok()?;
    if record.version != RECORD_VERSION || record.session_id.is_empty() {
        return None;
    }
    Some(record)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn attests_and_resolves_live_child_and_releases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn child");
        let child_pid = child.id().expect("child pid");
        let mut attestation = attest_session_spawn("sess-xyz", child_pid, Some(dir.path()));
        assert!(attestation.recorded().await);

        let resolved = resolve_pid_session(child_pid, Some(dir.path())).await;
        assert_eq!(
            resolved,
            PidSession::Attested {
                pid: child_pid,
                session_id: "sess-xyz".to_owned(),
                attested_pid: child_pid,
                host_pid: std::process::id(),
                chain: vec![child_pid],
            }
        );

        attestation.release().await;
        let after = resolve_pid_session(child_pid, Some(dir.path())).await;
        assert_eq!(
            after,
            PidSession::Unattested {
                pid: child_pid,
                reason: PidSessionReason::NoAttestedAncestor,
            }
        );
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}
