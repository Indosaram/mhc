//! senpi `modes/rpc/compact-root-owner.ts`: private recovery-root ownership markers,
//! boot-nonce verification, and no-follow stat/open helpers for dead-scratch reclamation.

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const ROOT_MARKER: &str = ".compact-spool-owner";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootMarkerKind {
    Scratch,
    Live,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootOwner {
    pub boot_nonce: String,
    pub pid: u32,
    pub start_time: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootMarker {
    pub kind: RootMarkerKind,
    pub token: String,
    pub owner: Option<RootOwner>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerState {
    Dead,
    Alive,
    Unknown,
}

pub async fn boot_nonce() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let text = tokio::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .await
            .ok()?;
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }
    #[cfg(target_os = "macos")]
    {
        let out = tokio::time::timeout(
            std::time::Duration::from_millis(2_000),
            tokio::process::Command::new("/usr/sbin/sysctl")
                .args(["-n", "kern.bootsessionuuid"])
                .output(),
        )
        .await
        .ok()?
        .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        (!s.is_empty()).then_some(s)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

pub async fn current_root_owner() -> Option<RootOwner> {
    let nonce = boot_nonce().await?;
    let pid = std::process::id();
    let start_time = crate::host_reservations::read_process_start_time(pid)?;
    Some(RootOwner {
        boot_nonce: nonce,
        pid,
        start_time,
    })
}

/// Only three proofs permit reclaim: different boot nonce, absent pid that is not live, or
/// mismatched start time on the same boot. A probe failure never becomes evidence of death.
pub async fn root_owner_state(owner: Option<&RootOwner>) -> OwnerState {
    let Some(owner) = owner else {
        return OwnerState::Unknown;
    };
    let Some(current_boot) = boot_nonce().await else {
        return OwnerState::Unknown;
    };
    if owner.boot_nonce != current_boot {
        return OwnerState::Dead;
    }
    match crate::host_reservations::read_process_start_time(owner.pid) {
        None => {
            if crate::host_reservations::process_is_live(owner.pid) {
                OwnerState::Unknown
            } else {
                OwnerState::Dead
            }
        }
        Some(start) => {
            if start == owner.start_time {
                OwnerState::Alive
            } else {
                OwnerState::Dead
            }
        }
    }
}

pub fn private_root_stat(metadata: &std::fs::Metadata) -> bool {
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = rustix::process::getuid().as_raw();
        metadata.uid() == uid && (metadata.mode() & 0o077) == 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The file is opened no-follow, bounded (<= 8,192 bytes), then parsed once; an old or
/// unreadable marker is `None` (UNKNOWN).
pub async fn read_root_marker(dir: &Path) -> Option<RootMarker> {
    let path = dir.join(ROOT_MARKER);
    let entry = tokio::fs::symlink_metadata(&path).await.ok()?;
    if !entry.is_file() || entry.file_type().is_symlink() {
        return None;
    }
    let mut opts = tokio::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts.open(&path).await.ok()?;
    let meta = file.metadata().await.ok()?;
    if !meta.is_file() || meta.len() > 8_192 {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = rustix::process::getuid().as_raw();
        if meta.uid() != uid || (meta.mode() & 0o077) != 0 {
            return None;
        }
    }
    let mut buf = String::new();
    use tokio::io::AsyncReadExt;
    let mut reader = file;
    reader.read_to_string(&mut buf).await.ok()?;
    let parsed: RootMarker = serde_json::from_str(&buf).ok()?;
    if parsed.token.is_empty() {
        return None;
    }
    if let Some(ref owner) = parsed.owner
        && (owner.boot_nonce.is_empty() || owner.pid == 0 || owner.start_time.is_empty())
    {
        return None;
    }
    Some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_valid_marker_and_rejects_oversized_or_group_readable() {
        let dir = tempfile::tempdir().expect("tempdir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
                .expect("chmod 700");
        }
        let meta = std::fs::symlink_metadata(dir.path()).expect("meta");
        assert!(private_root_stat(&meta));

        let marker_path = dir.path().join(ROOT_MARKER);
        let marker = RootMarker {
            kind: RootMarkerKind::Scratch,
            token: "tok-1".into(),
            owner: Some(RootOwner {
                boot_nonce: "old-boot".into(),
                pid: 123,
                start_time: "99".into(),
            }),
        };
        std::fs::write(&marker_path, serde_json::to_string(&marker).expect("json")).expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&marker_path, std::fs::Permissions::from_mode(0o600))
                .expect("chmod 600");
        }
        let read = read_root_marker(dir.path()).await.expect("read marker");
        assert_eq!(read, marker);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&marker_path, std::fs::Permissions::from_mode(0o644))
                .expect("chmod 644");
            assert!(read_root_marker(dir.path()).await.is_none());
        }
    }

    #[tokio::test]
    async fn root_owner_state_classifies_dead_boot_and_live_self() {
        assert_eq!(root_owner_state(None).await, OwnerState::Unknown);
        if let Some(self_owner) = current_root_owner().await {
            assert_eq!(root_owner_state(Some(&self_owner)).await, OwnerState::Alive);
            let mut stale_boot = self_owner;
            stale_boot.boot_nonce = "definitely-another-boot-nonce".into();
            assert_eq!(root_owner_state(Some(&stale_boot)).await, OwnerState::Dead);
        }
    }
}
