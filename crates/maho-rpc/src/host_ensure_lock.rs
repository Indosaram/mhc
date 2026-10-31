//! The ensure lock of one endpoint (senpi `rpc/host-ensure-lock.ts`): the one lock every operation that
//! changes WHO serves a socket takes first (`ensureHost`, `handoffHost`, `host gc`). It is keyed by the
//! socket's transport address rather than by any agent directory, and on POSIX by the canonical spelling
//! of that address, so two installations or two spellings of one socket exclude each other.
use crate::host_endpoint_names::canonical_endpoint_path;
use crate::ownership_safe_lock::{
    LockError, LockRetries, OwnershipSafeLock, acquire_ownership_safe_lock,
};
use crate::socket_transport::resolve_socket_transport_address;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::Duration;

/// Each SQLite busy wait stays short: it blocks the loop, so a same-process holder could never finish.
const LOCK_BUSY_WAIT_MS: u64 = 100;

/// The endpoint lock every ensure of `socket` serializes on, without its `.lock` suffix.
pub fn host_ensure_lock_target(socket: &str, platform: &str) -> PathBuf {
    std::env::temp_dir()
        .join("senpi-rpc-host-locks")
        .join(create_socket_lock_name(socket, platform))
}

pub async fn acquire_host_ensure_lock(
    socket: &str,
    wait_ms: u64,
    platform: &str,
) -> Result<OwnershipSafeLock, LockError> {
    let target = host_ensure_lock_target(socket, platform);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(&target)?;
    let mut lock_path = target.into_os_string();
    lock_path.push(".lock");
    acquire_ownership_safe_lock(&PathBuf::from(lock_path), host_ensure_lock_options(wait_ms)).await
}

pub fn host_ensure_lock_options(wait_ms: u64) -> LockRetries {
    LockRetries {
        retries: wait_ms.div_ceil(LOCK_BUSY_WAIT_MS) as u32,
        min_timeout: Duration::from_millis(20),
        max_timeout: Duration::from_millis(LOCK_BUSY_WAIT_MS),
    }
}

fn create_socket_lock_name(socket: &str, platform: &str) -> String {
    hex::encode(Sha256::digest(
        socket_lock_address(socket, platform).as_bytes(),
    ))[..32]
        .to_owned()
}

/// One address per PHYSICAL socket: the canonical endpoint path on POSIX; named pipes and abstract
/// sockets have no directory to resolve.
fn socket_lock_address(socket: &str, platform: &str) -> String {
    if platform == "win32" || socket.starts_with('\0') {
        return resolve_socket_transport_address(socket, platform, None)
            .unwrap_or_else(|_| socket.to_owned());
    }
    canonical_endpoint_path(socket, platform)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_spellings_of_one_socket_share_one_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plain = dir.path().join("h.sock").to_string_lossy().into_owned();
        let dotted = format!("{}/./h.sock", dir.path().display());
        assert_eq!(
            host_ensure_lock_target(&plain, "linux"),
            host_ensure_lock_target(&dotted, "linux")
        );
        let name = host_ensure_lock_target(&plain, "linux");
        assert_eq!(name.file_name().map(|name| name.len()), Some(32));
    }

    #[test]
    fn retries_cover_the_wait_in_busy_slices() {
        let options = host_ensure_lock_options(2_050);
        assert_eq!(options.retries, 21);
        assert_eq!(options.max_timeout, Duration::from_millis(100));
    }

    #[tokio::test]
    async fn the_lock_is_acquired_and_released() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir
            .path()
            .join("lock-test.sock")
            .to_string_lossy()
            .into_owned();
        let mut lock = acquire_host_ensure_lock(&socket, 200, "linux")
            .await
            .expect("lock");
        lock.release().expect("release");
    }
}
