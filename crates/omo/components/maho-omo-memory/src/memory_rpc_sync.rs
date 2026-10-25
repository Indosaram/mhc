//! The `sync` field of the `omo.memory.status` snapshot (#10637).
//!
//! The same failure state the TUI footer reads, so the desktop shows what the TUI shows.
//! Omitted while this device does not sync the repository (sync off, signed out, or another
//! process holds the engine), and when the read fails.

use std::path::Path;
use serde::{Deserialize, Serialize};

use crate::sync_status_surface::{SyncStatusReader, SyncSurfaceStatus};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRpcSyncFailure {
    pub kind: String,
    pub reason: String,
    pub action: String,
    pub since_iso: String,
    pub attempts: u32,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRpcSync {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_synced_at_iso: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<MemoryRpcSyncFailure>,
}

pub async fn read_memory_rpc_sync(
    read: Option<&SyncStatusReader>,
    repo: &Path,
) -> Option<MemoryRpcSync> {
    let read = read?;
    let status = read(repo).await?;
    match status {
        SyncSurfaceStatus::NeverSynced => Some(MemoryRpcSync {
            state: "never_synced".to_string(),
            last_synced_at_iso: None,
            failure: None,
        }),
        SyncSurfaceStatus::Paused => Some(MemoryRpcSync {
            state: "paused".to_string(),
            last_synced_at_iso: None,
            failure: None,
        }),
        SyncSurfaceStatus::Synced => Some(MemoryRpcSync {
            state: "synced".to_string(),
            last_synced_at_iso: Some(chrono::Utc::now().to_rfc3339()),
            failure: None,
        }),
        SyncSurfaceStatus::Unavailable | SyncSurfaceStatus::ConfigInvalid { .. } => {
            Some(MemoryRpcSync {
                state: "unavailable".to_string(),
                last_synced_at_iso: None,
                failure: None,
            })
        }
        SyncSurfaceStatus::Failing(failure) => Some(MemoryRpcSync {
            state: "failing".to_string(),
            last_synced_at_iso: None,
            failure: Some(MemoryRpcSyncFailure {
                kind: failure.kind,
                reason: failure.reason,
                action: failure.action,
                since_iso: chrono::Utc::now().to_rfc3339(),
                attempts: 1,
                paths: failure.paths,
            }),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_read_memory_rpc_sync_synced() {
        let reader: SyncStatusReader = Arc::new(|_repo| Box::pin(async { Some(SyncSurfaceStatus::Synced) }));
        let result = read_memory_rpc_sync(Some(&reader), Path::new("/repo")).await.unwrap();
        assert_eq!(result.state, "synced");
        assert!(result.last_synced_at_iso.is_some());
        assert!(result.failure.is_none());
    }

    #[tokio::test]
    async fn test_read_memory_rpc_sync_none() {
        let result = read_memory_rpc_sync(None, Path::new("/repo")).await;
        assert!(result.is_none());
    }
}
