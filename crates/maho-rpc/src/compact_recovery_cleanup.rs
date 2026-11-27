//! senpi `modes/rpc/compact-recovery-cleanup.ts`: startup sweep of recovery scratch roots
//! whose owner process is provably dead (RULING-65/79).

use crate::compact_root_owner::{
    OwnerState, RootMarkerKind, private_root_stat, read_root_marker, root_owner_state,
};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnknownRoot {
    pub name: String,
    pub age_ms: u64,
    pub bytes: Option<u64>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReclaimFailure {
    pub name: String,
    pub operation: String,
    pub cause: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootSweep {
    pub removed: usize,
    pub unknown: Vec<UnknownRoot>,
    pub failures: Vec<ReclaimFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

fn is_recovery_root_name(name: &str) -> bool {
    name.starts_with("senpi-transcript-")
        || name.starts_with("senpi-settled-")
        || name.starts_with("senpi-current-")
        || name.starts_with("senpi-event-leases-")
}

/// No-follow size accounting: a symlink's entry is counted, never its target.
fn root_bytes(path: &Path) -> BoxFuture<'_, Option<u64>> {
    Box::pin(async move {
        let meta = tokio::fs::symlink_metadata(path).await.ok()?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Some(meta.len());
        }
        let mut dir = tokio::fs::read_dir(path).await.ok()?;
        let mut total = 0u64;
        while let Some(entry) = dir.next_entry().await.ok()? {
            let child = root_bytes(&entry.path()).await?;
            total = total.saturating_add(child);
        }
        Some(total)
    })
}

fn age_ms(meta: &std::fs::Metadata) -> u64 {
    let Ok(mtime) = meta.modified() else {
        return 0;
    };
    std::time::SystemTime::now()
        .duration_since(mtime)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(unix)]
fn same_identity(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_identity(_: &std::fs::Metadata, _: &std::fs::Metadata) -> bool {
    true
}

/// RULING-65/79: only direct producer children of `base`, and only scratch whose owner is proven dead.
pub async fn sweep_recovery_roots(base: &Path) -> Result<RootSweep, std::io::Error> {
    if cfg!(windows) {
        return Ok(RootSweep {
            removed: 0,
            unknown: Vec::new(),
            failures: Vec::new(),
            skipped: Some("windows_owner_and_nofollow_unavailable".into()),
        });
    }
    let owned_base = tokio::fs::canonicalize(base)
        .await
        .unwrap_or_else(|_| base.to_path_buf());
    let mut entries = tokio::fs::read_dir(&owned_base).await?;
    let mut unknown = Vec::new();
    let mut failures = Vec::new();
    let mut removed = 0usize;

    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_recovery_root_name(&name) {
            continue;
        }
        let path = owned_base.join(&name);
        if path.parent() != Some(owned_base.as_path()) {
            continue;
        }
        let stat = match tokio::fs::symlink_metadata(&path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                failures.push(ReclaimFailure {
                    name,
                    operation: "reclaim".into(),
                    cause: e.to_string(),
                });
                continue;
            }
        };
        let marker = if private_root_stat(&stat) {
            read_root_marker(&path).await
        } else {
            None
        };
        let owner_state = match &marker {
            Some(m) => root_owner_state(m.owner.as_ref()).await,
            None => OwnerState::Unknown,
        };
        match (&marker, owner_state) {
            (Some(m), OwnerState::Dead) if m.kind == RootMarkerKind::Scratch => {
                let current = match tokio::fs::symlink_metadata(&path).await {
                    Ok(c) => c,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => {
                        failures.push(ReclaimFailure {
                            name,
                            operation: "reclaim".into(),
                            cause: e.to_string(),
                        });
                        continue;
                    }
                };
                if !same_identity(&stat, &current) || !private_root_stat(&current) {
                    failures.push(ReclaimFailure {
                        name,
                        operation: "reclaim".into(),
                        cause: "root identity changed during classification".into(),
                    });
                    continue;
                }
                match tokio::fs::remove_dir_all(&path).await {
                    Ok(()) => removed += 1,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => failures.push(ReclaimFailure {
                        name,
                        operation: "reclaim".into(),
                        cause: e.to_string(),
                    }),
                }
            }
            (None, _) | (_, OwnerState::Unknown) => {
                unknown.push(UnknownRoot {
                    name,
                    age_ms: age_ms(&stat),
                    bytes: root_bytes(&path).await,
                    reason: "ownership_unknown".into(),
                });
            }
            (Some(m), OwnerState::Dead) if m.kind == RootMarkerKind::Live => {
                unknown.push(UnknownRoot {
                    name,
                    age_ms: age_ms(&stat),
                    bytes: root_bytes(&path).await,
                    reason: "live_owner_dead".into(),
                });
            }
            _ => {}
        }
    }
    Ok(RootSweep {
        removed,
        unknown,
        failures,
        skipped: None,
    })
}

/// Startup errors stay visible and counted; they never prevent the host from accepting other work.
pub async fn start_recovery_cleanup(base: &Path) -> RootSweep {
    let result = match sweep_recovery_roots(base).await {
        Ok(sweep) => sweep,
        Err(err) => RootSweep {
            removed: 0,
            unknown: Vec::new(),
            failures: vec![ReclaimFailure {
                name: ".".into(),
                operation: "reclaim".into(),
                cause: err.to_string(),
            }],
            skipped: None,
        },
    };
    if !result.unknown.is_empty() || !result.failures.is_empty() || result.skipped.is_some() {
        eprintln!(
            "senpi rpc recovery roots: {}",
            serde_json::to_string(&result).unwrap_or_default()
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_root_owner::{ROOT_MARKER, RootMarker, RootOwner, current_root_owner};

    #[cfg(unix)]
    #[tokio::test]
    async fn removes_dead_scratch_preserves_live_and_reports_unknown() {
        use std::os::unix::fs::PermissionsExt;

        let base = tempfile::tempdir().expect("tempdir");

        let make_root = |name: &str, marker: Option<&RootMarker>| {
            let dir = base.path().join(name);
            std::fs::create_dir(&dir).expect("mkdir");
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
            if let Some(m) = marker {
                let mp = dir.join(ROOT_MARKER);
                std::fs::write(&mp, serde_json::to_string(m).expect("json")).expect("write");
                std::fs::set_permissions(&mp, std::fs::Permissions::from_mode(0o600))
                    .expect("chmod");
            }
            dir
        };

        let dead_scratch = make_root(
            "senpi-settled-dead",
            Some(&RootMarker {
                kind: RootMarkerKind::Scratch,
                token: "t1".into(),
                owner: Some(RootOwner {
                    boot_nonce: "stale-boot".into(),
                    pid: 1,
                    start_time: "1".into(),
                }),
            }),
        );
        let dead_live = make_root(
            "senpi-current-deadlive",
            Some(&RootMarker {
                kind: RootMarkerKind::Live,
                token: "t2".into(),
                owner: Some(RootOwner {
                    boot_nonce: "stale-boot".into(),
                    pid: 1,
                    start_time: "1".into(),
                }),
            }),
        );
        let no_marker = make_root("senpi-transcript-nomarker", None);
        let unrelated = make_root("other-dir", None);

        let self_owner = current_root_owner().await;
        let alive_dir = self_owner.as_ref().map(|owner| {
            make_root(
                "senpi-event-leases-alive",
                Some(&RootMarker {
                    kind: RootMarkerKind::Scratch,
                    token: "t3".into(),
                    owner: Some(owner.clone()),
                }),
            )
        });

        let sweep = sweep_recovery_roots(base.path()).await.expect("sweep");
        assert_eq!(sweep.removed, 1);
        assert!(!dead_scratch.exists());
        assert!(dead_live.exists());
        assert!(no_marker.exists());
        assert!(unrelated.exists());
        if let Some(alive) = alive_dir {
            assert!(alive.exists());
        }
        assert_eq!(sweep.unknown.len(), 2);
    }
}
