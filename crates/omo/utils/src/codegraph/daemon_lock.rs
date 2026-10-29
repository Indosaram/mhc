use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::paths::canonicalize_codegraph_path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DaemonStalenessReason {
    #[serde(rename = "lock-absent")]
    LockAbsent,
    #[serde(rename = "lock-pid-mismatch")]
    LockPidMismatch,
    #[serde(rename = "lock-pid-match")]
    LockPidMatch,
    #[serde(rename = "lock-unparseable")]
    LockUnparseable,
    #[serde(rename = "lock-unreadable")]
    LockUnreadable,
}

impl DaemonStalenessReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LockAbsent => "lock-absent",
            Self::LockPidMismatch => "lock-pid-mismatch",
            Self::LockPidMatch => "lock-pid-match",
            Self::LockUnparseable => "lock-unparseable",
            Self::LockUnreadable => "lock-unreadable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodegraphDaemonStaleness {
    pub stale: bool,
    pub reason: DaemonStalenessReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodegraphDaemonLock {
    pub pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socket_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

pub fn parse_daemon_lock(raw: &str) -> Option<CodegraphDaemonLock> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
        && let Some(obj) = value.as_object()
        && let Some(pid_val) = obj.get("pid")
        && let Some(pid_num) = pid_val.as_u64()
        && pid_num > 0
        && pid_num <= u32::MAX as u64
    {
        let pid = pid_num as u32;
        let socket_path = obj
            .get("socketPath")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let started_at = obj.get("startedAt").and_then(|v| v.as_u64());
        let version = obj
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        return Some(CodegraphDaemonLock {
            pid,
            socket_path,
            started_at,
            version,
        });
    }

    if let Ok(legacy_pid) = trimmed.parse::<u32>()
        && legacy_pid > 0
    {
        return Some(CodegraphDaemonLock {
            pid: legacy_pid,
            socket_path: None,
            started_at: None,
            version: None,
        });
    }

    None
}

fn collect_ancestors(start: &Path, output: &mut BTreeSet<PathBuf>) {
    let mut current = start.to_path_buf();
    loop {
        output.insert(current.clone());
        match current.parent() {
            Some(parent) if parent != current => {
                current = parent.to_path_buf();
            }
            _ => break,
        }
    }
}

pub fn daemon_lock_candidates(project_root: &Path) -> Vec<PathBuf> {
    let resolved = if project_root.is_absolute() {
        project_root.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_default()
            .join(project_root)
    };

    let mut dirs = BTreeSet::new();
    collect_ancestors(&resolved, &mut dirs);
    let real = canonicalize_codegraph_path(&resolved);
    collect_ancestors(&real, &mut dirs);

    dirs.into_iter()
        .map(|dir| dir.join(".codegraph").join("daemon.pid"))
        .collect()
}

pub fn evaluate_daemon_staleness(pid: u32, project_root: &Path) -> CodegraphDaemonStaleness {
    let mut saw_lock = false;
    for lock_path in daemon_lock_candidates(project_root) {
        let raw = match fs::read_to_string(&lock_path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                #[cfg(unix)]
                {
                    if e.raw_os_error() == Some(libc::ENOTDIR) {
                        continue;
                    }
                }
                let _ = e;
                return CodegraphDaemonStaleness {
                    stale: false,
                    reason: DaemonStalenessReason::LockUnreadable,
                };
            }
        };

        saw_lock = true;
        let Some(lock) = parse_daemon_lock(&raw) else {
            return CodegraphDaemonStaleness {
                stale: false,
                reason: DaemonStalenessReason::LockUnparseable,
            };
        };

        if lock.pid == pid {
            return CodegraphDaemonStaleness {
                stale: false,
                reason: DaemonStalenessReason::LockPidMatch,
            };
        }
    }

    if saw_lock {
        CodegraphDaemonStaleness {
            stale: true,
            reason: DaemonStalenessReason::LockPidMismatch,
        }
    } else {
        CodegraphDaemonStaleness {
            stale: true,
            reason: DaemonStalenessReason::LockAbsent,
        }
    }
}
