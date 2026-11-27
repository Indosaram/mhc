//! Who owns an endpoint's public socket entry, and the fence every rename or remove of that entry
//! runs under (senpi#4602, A-amended section 4, carrying the senpi#2729 ordering rule into endpoint
//! adoption).
//!
//! The owner token names the generation that serves the public name. The fence is a short, cross-process
//! exclusive SQLite lock keyed by the canonical endpoint, the same key the ensure lock uses, but a
//! different file: a predecessor's teardown must be able to finish its own short mutation while a
//! successor's launcher holds the long ensure lock.

use crate::host_ensure_lock::{host_ensure_lock_options, host_ensure_lock_target};
use crate::ownership_safe_lock::{LockError, LockRetries, acquire_ownership_safe_lock};
use crate::socket_ownership::{
    SocketFileIdentity, adopt_public_socket, shield_socket_during_close, unlink_owned_socket,
};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// P6's allowance: a fenced mutation that cannot start within it refuses without mutating anything.
pub const ENDPOINT_FENCE_WAIT_MS: u64 = 15_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointOwnerToken {
    pub v: u32,
    pub endpoint: String,
    pub instance_id: String,
    pub generation: u64,
    pub pid: u32,
    /// Fresh per claim: two claims by one generation are still two different tokens.
    pub nonce: String,
}

#[derive(Default)]
pub struct FenceHooks {
    /// Test seam: the fence was held by another writer when this one asked for it.
    pub on_fence_busy: Option<Box<dyn FnOnce() + Send>>,
}

pub fn endpoint_owner_token(
    endpoint: &str,
    instance_id: &str,
    generation: u64,
) -> EndpointOwnerToken {
    EndpointOwnerToken {
        v: 1,
        endpoint: endpoint.to_owned(),
        instance_id: instance_id.to_owned(),
        generation,
        pid: std::process::id(),
        nonce: uuid::Uuid::new_v4().to_string(),
    }
}

fn target_with_suffix(socket: &str, platform: &str, suffix: &str) -> PathBuf {
    let mut os = host_ensure_lock_target(socket, platform).into_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

fn owner_file(socket: &str, platform: &str) -> PathBuf {
    target_with_suffix(socket, platform, ".owner.json")
}

fn fence_lock_file(socket: &str, platform: &str) -> PathBuf {
    target_with_suffix(socket, platform, ".fence.lock")
}

fn is_lock_busy(error: &LockError) -> bool {
    match error {
        LockError::Sqlite(sqlite_err) => {
            sqlite_err.sqlite_error().is_some_and(|e| {
                matches!(
                    e.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                )
            }) || sqlite_err.to_string().to_lowercase().contains("busy")
                || sqlite_err.to_string().to_lowercase().contains("locked")
        }
        _ => false,
    }
}

async fn with_endpoint_fence<T>(
    socket: &str,
    platform: &str,
    hooks: Option<FenceHooks>,
    run: impl std::future::Future<Output = io::Result<T>>,
) -> io::Result<T> {
    let fence = fence_lock_file(socket, platform);
    if let Some(parent) = fence.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let immediate = LockRetries {
        retries: 0,
        min_timeout: Duration::from_millis(1),
        max_timeout: Duration::from_millis(1),
    };
    let mut lock = match acquire_ownership_safe_lock(&fence, immediate).await {
        Ok(lock) => lock,
        Err(err) if is_lock_busy(&err) => {
            if let Some(mut hooks) = hooks
                && let Some(on_busy) = hooks.on_fence_busy.take()
            {
                on_busy();
            }
            acquire_ownership_safe_lock(&fence, host_ensure_lock_options(ENDPOINT_FENCE_WAIT_MS))
                .await
                .map_err(io::Error::other)?
        }
        Err(err) => return Err(io::Error::other(err)),
    };
    let result = run.await;
    let _ = lock.release();
    result
}

/// The current owner token; `None` when none was claimed or it cannot be read as one.
pub fn read_endpoint_owner(socket: &str, platform: &str) -> Option<EndpointOwnerToken> {
    let text = std::fs::read_to_string(owner_file(socket, platform)).ok()?;
    let token: EndpointOwnerToken = serde_json::from_str(&text).ok()?;
    (token.v == 1).then_some(token)
}

fn write_endpoint_owner(
    socket: &str,
    platform: &str,
    token: &EndpointOwnerToken,
) -> io::Result<()> {
    let path = owner_file(socket, platform);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let staged = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&staged)?;
    let content = format!(
        "{}\n",
        serde_json::to_string(token).map_err(io::Error::other)?
    );
    std::io::Write::write_all(&mut file, content.as_bytes())?;
    std::fs::rename(staged, path)
}

/// An initial start: binds the public name (`bind`, with its stale-entry preparation) and claims it.
pub async fn claim_endpoint(
    socket: &str,
    platform: &str,
    token: EndpointOwnerToken,
    bind: Option<BoxFuture<'static, io::Result<()>>>,
) -> io::Result<()> {
    if read_endpoint_owner(socket, platform).is_some() {
        with_endpoint_fence(socket, platform, None, async {
            if let Some(bind_fut) = bind {
                bind_fut.await?;
            }
            write_endpoint_owner(socket, platform, &token)
        })
        .await
    } else {
        if let Some(bind_fut) = bind {
            bind_fut.await?;
        }
        write_endpoint_owner(socket, platform, &token)
    }
}

pub struct AdoptEndpointOptions {
    pub bind_socket: String,
    pub public_socket: String,
    pub platform: String,
    pub expected: Option<SocketFileIdentity>,
    /// The predecessor's `instanceId`; `None` for a predecessor that does not advertise the fence (senpi#4602 Q1).
    pub expected_owner: Option<String>,
    pub token: EndpointOwnerToken,
    /// Whether the launcher's publication grant still stands; absent for an uncoordinated launch.
    pub granted: Option<Box<dyn FnOnce() -> BoxFuture<'static, bool> + Send>>,
    pub hooks: Option<FenceHooks>,
}

/// A successor takes the public name over. Inside the fence: the launcher's grant must still stand,
/// the owner token must still name the expected predecessor (when the predecessor participates in the
/// fence at all), the entry must still be the one the handoff was decided against; then the rename
/// and the new token, awaited.
pub async fn adopt_endpoint(mut options: AdoptEndpointOptions) -> io::Result<()> {
    let public_socket = options.public_socket.clone();
    let platform = options.platform.clone();
    with_endpoint_fence(&public_socket, &platform, options.hooks.take(), async {
        if let Some(granted) = options.granted.take()
            && !granted().await
        {
            return Err(io::Error::other(format!(
                "{public_socket}: the launcher revoked this generation's adoption grant"
            )));
        }
        let current = read_endpoint_owner(&public_socket, &platform);
        if let Some(expected_owner) = &options.expected_owner
            && let Some(cur) = current
            && cur.instance_id != *expected_owner
        {
            return Err(io::Error::other(format!(
                "{public_socket}: owner token names {}, not {expected_owner}; refusing to replace it",
                cur.instance_id
            )));
        }
        adopt_public_socket(
            Path::new(&options.bind_socket),
            Path::new(&public_socket),
            options.expected,
        )?;
        write_endpoint_owner(&public_socket, &platform, &options.token)
    })
    .await
}

/// A generation removes its public entry: only while the owner token does not name another generation,
/// and only the entry `identity` describes. Its own token goes with it.
pub async fn unlink_owned_endpoint(
    socket: &str,
    platform: &str,
    identity: Option<SocketFileIdentity>,
    instance_id: &str,
    mut log: impl FnMut(String),
    after_validate: Option<BoxFuture<'static, ()>>,
) -> io::Result<()> {
    with_endpoint_fence(socket, platform, None, async {
        let current = read_endpoint_owner(socket, platform);
        if let Some(cur) = &current
            && cur.instance_id != instance_id
        {
            log(format!(
                "socket path {socket} is owned by generation {} now; leaving it",
                cur.instance_id
            ));
            return Ok(());
        }
        if let Some(hook) = after_validate {
            hook.await;
        }
        unlink_owned_socket(socket, identity, platform, &mut log);
        if current.is_some() {
            match std::fs::remove_file(owner_file(socket, platform)) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        Ok(())
    })
    .await
}

/// Closes a listener bound to `socket` with the entry shielded from close-time unlink of the NAME.
pub async fn close_shielded_endpoint<T>(
    socket: &str,
    platform: &str,
    close: impl std::future::Future<Output = T>,
) -> io::Result<T> {
    with_endpoint_fence(socket, platform, None, async {
        Ok(shield_socket_during_close(socket, platform, close).await)
    })
    .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdoptionSettlement {
    Completed,
    NotAdopted,
}

/// A successor's launcher, after its attempt failed: revoke the publication grant, then read who owns
/// the public name.
pub async fn settle_candidate_adoption(
    socket: &str,
    platform: &str,
    candidate: &str,
    revoke: impl std::future::Future<Output = ()>,
) -> io::Result<AdoptionSettlement> {
    with_endpoint_fence(socket, platform, None, async {
        revoke.await;
        Ok(
            if read_endpoint_owner(socket, platform)
                .is_some_and(|owner| owner.instance_id == candidate)
            {
                AdoptionSettlement::Completed
            } else {
                AdoptionSettlement::NotAdopted
            },
        )
    })
    .await
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::socket_ownership::{generation_bind_path, stat_socket_identity};
    use std::os::unix::net::UnixListener;

    struct CleanupOwnerFiles(String);
    impl Drop for CleanupOwnerFiles {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(owner_file(&self.0, "linux"));
            let _ = std::fs::remove_file(fence_lock_file(&self.0, "linux"));
        }
    }

    #[tokio::test]
    async fn adoption_moves_public_name_and_blocks_foreign_owner_or_late_predecessor_unlink() {
        let temp = tempfile::tempdir().expect("tempdir");
        let public = temp.path().join("p.sock").to_string_lossy().into_owned();
        let bind = generation_bind_path(&public, 1);
        let _cleanup = CleanupOwnerFiles(public.clone());

        let l_pred = UnixListener::bind(&public).expect("bind pred");
        let pred_id = stat_socket_identity(Path::new(&public)).expect("stat");
        claim_endpoint(
            &public,
            "linux",
            endpoint_owner_token(&public, "pred", 0),
            None,
        )
        .await
        .expect("claim");

        let l_succ = UnixListener::bind(&bind).expect("bind succ");
        let succ_id = stat_socket_identity(Path::new(&bind)).expect("stat succ");

        // Adoption succeeds and moves owner token to succ.
        adopt_endpoint(AdoptEndpointOptions {
            bind_socket: bind.clone(),
            public_socket: public.clone(),
            platform: "linux".into(),
            expected: pred_id,
            expected_owner: Some("pred".into()),
            token: endpoint_owner_token(&public, "succ", 1),
            granted: None,
            hooks: None,
        })
        .await
        .expect("adopt");
        assert_eq!(
            stat_socket_identity(Path::new(&public)).expect("stat"),
            succ_id
        );
        assert_eq!(
            read_endpoint_owner(&public, "linux")
                .expect("owner")
                .instance_id,
            "succ"
        );

        // Late predecessor unlink leaves successor's socket and token intact.
        unlink_owned_endpoint(&public, "linux", succ_id, "pred", |_| {}, None)
            .await
            .expect("unlink");
        assert_eq!(
            stat_socket_identity(Path::new(&public)).expect("stat"),
            succ_id
        );
        assert_eq!(
            read_endpoint_owner(&public, "linux")
                .expect("owner")
                .instance_id,
            "succ"
        );

        // Shielded close restores successor's entry.
        close_shielded_endpoint(&public, "linux", async {
            assert!(!Path::new(&public).exists());
        })
        .await
        .expect("shield");
        assert_eq!(
            stat_socket_identity(Path::new(&public)).expect("stat"),
            succ_id
        );

        drop((l_pred, l_succ));
    }

    #[tokio::test]
    async fn adoption_refuses_revoked_grant_and_settle_reports_outcome() {
        let temp = tempfile::tempdir().expect("tempdir");
        let public = temp.path().join("p2.sock").to_string_lossy().into_owned();
        let bind = generation_bind_path(&public, 1);
        let _cleanup = CleanupOwnerFiles(public.clone());

        let l_pred = UnixListener::bind(&public).expect("bind pred");
        let pred_id = stat_socket_identity(Path::new(&public)).expect("stat");
        claim_endpoint(
            &public,
            "linux",
            endpoint_owner_token(&public, "pred", 0),
            None,
        )
        .await
        .expect("claim");
        let l_succ = UnixListener::bind(&bind).expect("bind succ");

        let outcome = settle_candidate_adoption(&public, "linux", "succ", async {})
            .await
            .expect("settle");
        assert_eq!(outcome, AdoptionSettlement::NotAdopted);

        let err = adopt_endpoint(AdoptEndpointOptions {
            bind_socket: bind.clone(),
            public_socket: public.clone(),
            platform: "linux".into(),
            expected: pred_id,
            expected_owner: Some("pred".into()),
            token: endpoint_owner_token(&public, "succ", 1),
            granted: Some(Box::new(|| Box::pin(async { false }))),
            hooks: None,
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("revoked"));

        // After a real adoption, settle reports Completed.
        adopt_endpoint(AdoptEndpointOptions {
            bind_socket: bind,
            public_socket: public.clone(),
            platform: "linux".into(),
            expected: pred_id,
            expected_owner: Some("pred".into()),
            token: endpoint_owner_token(&public, "succ", 1),
            granted: Some(Box::new(|| Box::pin(async { true }))),
            hooks: None,
        })
        .await
        .expect("adopt");
        assert_eq!(
            settle_candidate_adoption(&public, "linux", "succ", async {})
                .await
                .expect("settle"),
            AdoptionSettlement::Completed
        );
        drop((l_pred, l_succ));
    }
}
