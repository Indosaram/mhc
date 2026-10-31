//! The supervisor's PUBLIC socket proxy (senpi `host-lifecycle-proxy.ts`): every client reaches the host through it.
//! The proxy is sealed-hop aware (senpi#3795): the internal side is connected with the supervisor's memory-only key,
//! and a POSIX public connection is filtered by its `RpcPublicEndpoint` before admission. Draining peers can still
//! probe and receive typed open refusals without a child hop.
#[cfg(unix)]
use crate::host_admission::HostAdmission;
#[cfg(unix)]
use crate::internal_channel::connect_internal_connection;
#[cfg(unix)]
use crate::rpc_public_key::RpcPublicEndpoint;
#[cfg(unix)]
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

/// Either side of a hop once admitted: a sealed record stream, a filtered public view, or a raw socket.
pub trait HopStream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> HopStream for T {}
pub type BoxedHop = Box<dyn HopStream>;

/// One sealed readiness probe: the internal handshake and the ready reply together, on one monotonic timer
/// (senpi#3816 review). A child that accepts the hop and stays silent that long is not a working host.
pub const INTERNAL_READINESS_PROBE_MS: u64 = 10_000;

#[cfg(unix)]
#[derive(Clone)]
pub struct PublicProxyOptions {
    pub internal_socket: std::path::PathBuf,
    pub internal_key: Option<[u8; 32]>,
    pub public_endpoint: Option<Arc<RpcPublicEndpoint>>,
    pub require_token: bool,
    /// Shared authority for stopping, protocol replies and new-session admission.
    pub admission: Arc<HostAdmission>,
}

/// Opens the internal side of one public client: `None` while stopping, the sealed stream when a key is set.
#[cfg(unix)]
async fn open_internal(options: &PublicProxyOptions) -> std::io::Result<Option<BoxedHop>> {
    if options.admission.stopping() {
        return Ok(None);
    }
    let internal = tokio::net::UnixStream::connect(&options.internal_socket).await?;
    Ok(Some(match options.internal_key {
        // The proxy forwarding a public connection passes no deadline: a host whose loop stalls still serves the
        // connections made meanwhile.
        Some(key) => Box::new(
            connect_internal_connection(internal, &key, None)
                .await
                .map_err(|error| std::io::Error::other(error.0))?,
        ) as BoxedHop,
        None => Box::new(internal) as BoxedHop,
    }))
}

/// One accepted public client (senpi `createPublicProxy`'s connection handler).
#[cfg(unix)]
pub async fn proxy_public_client(
    client: tokio::net::UnixStream,
    options: PublicProxyOptions,
) -> std::io::Result<()> {
    let hop = open_internal(&options).await?;
    let client: BoxedHop = match &options.public_endpoint {
        Some(endpoint) => Box::new(crate::rpc_public_auth::public_rpc_stream(
            client,
            endpoint.clone(),
            options.require_token,
        )),
        None => Box::new(client),
    };
    crate::host_admission_proxy::proxy_with_admission(client, hop, options.admission.clone()).await;
    Ok(())
}

/// senpi `createPublicProxy` + its listener: accepts until draining, then waits for the clients it holds.
#[cfg(unix)]
pub async fn run_public_proxy(
    listener: tokio::net::UnixListener,
    options: PublicProxyOptions,
    mut draining: tokio::sync::watch::Receiver<bool>,
    connections: tokio::sync::watch::Sender<usize>,
) -> std::io::Result<()> {
    let mut clients = tokio::task::JoinSet::new();
    loop {
        if *draining.borrow() {
            break;
        }
        tokio::select! {
            biased;
            changed = draining.changed() => { if changed.is_err() || *draining.borrow() { break; } },
            result = clients.join_next(), if !clients.is_empty() => { let _ = result; connections.send_replace(clients.len()); },
            accepted = listener.accept() => {
                let (client, _) = accepted?;
                if *draining.borrow() { break; }
                clients.spawn(proxy_public_client(client, options.clone()));
                connections.send_replace(clients.len());
            }
        }
    }
    drop(listener);
    while clients.join_next().await.is_some() {
        connections.send_replace(clients.len());
    }
    Ok(())
}

/// One entry's identity including its change time: a rebind at the same inode still moves ctime.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EntryStamp {
    dev: u64,
    ino: u64,
    ctime_ns: i128,
    socket: bool,
}

#[cfg(unix)]
fn stamp_of(socket_path: &str) -> std::io::Result<Option<EntryStamp>> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    match std::fs::metadata(socket_path) {
        Ok(entry) => Ok(Some(EntryStamp {
            dev: entry.dev(),
            ino: entry.ino(),
            ctime_ns: i128::from(entry.ctime()) * 1_000_000_000 + i128::from(entry.ctime_nsec()),
            socket: entry.file_type().is_socket(),
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// senpi `prepareSocketPath`: makes `socket_path` bindable without ever removing a live listener's entry. A path the
/// kernel would truncate is refused; a dead socket entry is unlinked only while it is still the entry probed dead.
#[cfg(unix)]
pub async fn prepare_socket_path(socket_path: &str) -> std::io::Result<()> {
    prepare_socket_path_with(
        socket_path,
        async |socket: &str| {
            crate::socket_ownership::socket_entry_listener_state(
                socket,
                std::env::consts::OS,
                crate::socket_ownership::lsof_socket_holders,
            )
            .await
        },
        async || {},
    )
    .await
}

/// [`prepare_socket_path`] with its test seams (senpi `PrepareSocketPathHooks`): `listener_state` replaces the
/// listener probe; `after_stale_probe` runs after the entry was probed dead and before it is unlinked.
#[cfg(unix)]
pub async fn prepare_socket_path_with(
    socket_path: &str,
    listener_state: impl AsyncFnOnce(&str) -> crate::socket_ownership::ListenerState,
    after_stale_probe: impl AsyncFnOnce(),
) -> std::io::Result<()> {
    use crate::socket_ownership::{ListenerVerdict, MAX_SOCKET_PATH_BYTES};
    if socket_path.starts_with('\0') {
        return Ok(());
    }
    // A path the kernel would truncate binds a DIFFERENT endpoint than the one every client was told about.
    if socket_path.len() > MAX_SOCKET_PATH_BYTES {
        return Err(std::io::Error::other(format!(
            "{socket_path}: socket path is {} bytes, over the {MAX_SOCKET_PATH_BYTES}-byte limit.",
            socket_path.len()
        )));
    }
    if let Some(parent) = std::path::Path::new(socket_path).parent() {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let Some(probed) = stamp_of(socket_path)? else {
        return Ok(());
    };
    let in_use = |evidence: &str| {
        std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("{socket_path}: address already in use by a live server ({evidence})."),
        )
    };
    // Linux refuses a connect to a regular file too; never remove what is not a socket.
    if !probed.socket {
        return Err(in_use("the entry is not a socket"));
    }
    let state = listener_state(socket_path).await;
    match state.verdict {
        ListenerVerdict::Gone => return Ok(()),
        ListenerVerdict::Live => return Err(in_use(&state.evidence)),
        ListenerVerdict::Dead => {}
    }
    after_stale_probe().await;
    // Something may have replaced the entry since the probe; unlink only the entry probed dead.
    let Some(current) = stamp_of(socket_path)? else {
        return Ok(());
    };
    if (probed.dev, probed.ino, probed.ctime_ns) != (current.dev, current.ino, current.ctime_ns) {
        return Err(in_use("the entry changed after it was probed"));
    }
    match std::fs::remove_file(socket_path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// senpi `listen` (POSIX): binds the public entry and makes it private to this user (0600).
#[cfg(unix)]
pub fn listen(socket_path: &str) -> std::io::Result<tokio::net::UnixListener> {
    let listener = tokio::net::UnixListener::bind(socket_path)?;
    if !socket_path.starts_with('\0') {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(listener)
}

/// What one readiness probe found (senpi `canConnect`).
#[derive(Debug, Clone, PartialEq)]
pub enum ReadinessProbe {
    /// The ready reply's `data`.
    Ready(serde_json::Value),
    /// Not listening yet: retried until the overall deadline.
    Refused,
    /// Accepted the hop and answered nothing within the probe bound.
    Silent,
    /// Answered but could not prove the key (`host_identity_unproven` / `auth_failed`): it will not start proving it.
    Unproven(&'static str),
}

/// One sealed readiness probe over the internal hop.
#[cfg(unix)]
pub async fn probe_internal_listener(
    socket_path: &std::path::Path,
    internal_key: &[u8; 32],
    probe_ms: u64,
) -> ReadinessProbe {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let Ok(socket) = tokio::net::UnixStream::connect(socket_path).await else {
        return ReadinessProbe::Refused;
    };
    let exchange = async {
        let mut stream = match connect_internal_connection(socket, internal_key, None).await {
            Ok(stream) => stream,
            Err(error) if matches!(error.0, "host_identity_unproven" | "auth_failed") => {
                return ReadinessProbe::Unproven(error.0);
            }
            Err(_) => return ReadinessProbe::Refused,
        };
        if stream
            .write_all(
                b"{\"id\":\"internal-ready\",\"type\":\"get_protocol_info\",\"observe\":true}\n",
            )
            .await
            .and(stream.flush().await)
            .is_err()
        {
            return ReadinessProbe::Refused;
        }
        let mut lines = tokio::io::BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(response) = serde_json::from_str::<serde_json::Value>(&line) else {
                return ReadinessProbe::Refused;
            };
            if response["id"] == "internal-ready" {
                return if response["success"] == true {
                    ReadinessProbe::Ready(response["data"].clone())
                } else {
                    ReadinessProbe::Refused
                };
            }
        }
        ReadinessProbe::Refused
    };
    // The connection was accepted: expiring now means the child is silent.
    tokio::time::timeout(std::time::Duration::from_millis(probe_ms), exchange)
        .await
        .unwrap_or(ReadinessProbe::Silent)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::{Map, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    const KEY: [u8; 32] = [7; 32];

    /// A listener that bound the path and went away: the entry stays, and connecting to it is refused.
    fn stale_entry(path: &std::path::Path) {
        drop(std::os::unix::net::UnixListener::bind(path).expect("bind"));
    }

    fn pinned(
        verdict: crate::socket_ownership::ListenerVerdict,
    ) -> crate::socket_ownership::ListenerState {
        crate::socket_ownership::ListenerState {
            verdict,
            evidence: format!("pinned {verdict:?}"),
        }
    }

    async fn prepare(path: &std::path::Path) -> std::io::Result<()> {
        prepare_socket_path(path.to_str().expect("utf8")).await
    }

    #[tokio::test]
    async fn removes_an_entry_nobody_listens_behind() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("rpc.sock");
        stale_entry(&path);
        prepare(&path).await.expect("prepared");
        assert!(!path.exists());
        // A missing entry (and its missing parent) is prepared, not refused.
        prepare(&dir.path().join("new").join("rpc.sock"))
            .await
            .expect("absent");
        assert!(dir.path().join("new").is_dir());
    }

    #[tokio::test]
    async fn acts_on_each_listener_verdict_live_keeps_dead_removes_gone_leaves() {
        use crate::socket_ownership::ListenerVerdict;
        let dir = tempfile::tempdir().expect("dir");
        let live = dir.path().join("live.sock");
        stale_entry(&live);
        let error = prepare_socket_path_with(
            live.to_str().expect("utf8"),
            async |_: &str| pinned(ListenerVerdict::Live),
            async || {},
        )
        .await
        .expect_err("live");
        assert!(error.to_string().contains("pinned Live"));
        assert!(live.exists());
        let dead = dir.path().join("dead.sock");
        stale_entry(&dead);
        prepare_socket_path_with(
            dead.to_str().expect("utf8"),
            async |_: &str| pinned(ListenerVerdict::Dead),
            async || {},
        )
        .await
        .expect("dead");
        assert!(!dead.exists());
        let gone = dir.path().join("gone.sock");
        stale_entry(&gone);
        prepare_socket_path_with(
            gone.to_str().expect("utf8"),
            async |_: &str| pinned(ListenerVerdict::Gone),
            async || {},
        )
        .await
        .expect("gone");
        assert!(gone.exists());
    }

    #[tokio::test]
    async fn refuses_a_path_a_live_listener_holds_and_keeps_it() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("rpc.sock");
        let _listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let error = prepare(&path).await.expect_err("live");
        assert!(error.to_string().contains("accepting connections"));
        assert!(tokio::net::UnixStream::connect(&path).await.is_ok());
    }

    // `libc::listen` re-arms the bound listener with a backlog of one; nothing else in the test is unsafe.
    #[allow(unsafe_code)]
    #[tokio::test]
    async fn keeps_a_live_listener_whose_backlog_is_full() {
        use std::os::fd::AsRawFd;
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("rpc.sock");
        // A live listener with a backlog of one that never accepts (re-listening lowers the backlog in place).
        let stuck = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        // SAFETY: `stuck` owns this listening descriptor for the whole call.
        assert_eq!(unsafe { libc::listen(stuck.as_raw_fd(), 1) }, 0);
        // Connect until the backlog is full: Linux answers EAGAIN, macOS and the BSDs refuse.
        let mut clients = Vec::new();
        let full = loop {
            assert!(clients.len() < 64, "the backlog never filled");
            match tokio::net::UnixStream::connect(&path).await {
                Ok(client) => clients.push(client),
                Err(error) => break error,
            }
        };
        assert_ne!(full.kind(), std::io::ErrorKind::NotFound);
        let error = prepare(&path).await.expect_err("live");
        assert!(
            error
                .to_string()
                .contains("address already in use by a live server")
        );
        assert!(path.symlink_metadata().is_ok());
        drop(clients);
        drop(stuck);
    }

    #[tokio::test]
    async fn keeps_an_entry_it_may_not_connect_to() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::getuid().is_root() {
            return;
        }
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("rpc.sock");
        let _listener = tokio::net::UnixListener::bind(&path).expect("bind");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let error = prepare(&path).await.expect_err("eacces");
        assert!(
            error
                .to_string()
                .contains("address already in use by a live server")
        );
        assert!(path.symlink_metadata().is_ok());
    }

    #[tokio::test]
    async fn never_removes_an_entry_that_is_not_a_socket() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("rpc.sock");
        std::fs::write(&path, "not a socket").expect("write");
        let error = prepare(&path).await.expect_err("regular file");
        assert!(error.to_string().contains("not a socket"));
        assert!(path.exists());
    }

    #[tokio::test]
    async fn never_removes_a_listener_that_bound_the_path_after_the_stale_probe() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("rpc.sock");
        stale_entry(&path);
        let rebound = std::sync::Mutex::new(None);
        let error = prepare_socket_path_with(
            path.to_str().expect("utf8"),
            async |socket: &str| {
                crate::socket_ownership::socket_entry_listener_state(
                    socket,
                    "linux",
                    crate::socket_ownership::lsof_socket_holders,
                )
                .await
            },
            async || {
                std::fs::remove_file(&path).expect("unlink");
                *rebound.lock().expect("lock") =
                    Some(std::os::unix::net::UnixListener::bind(&path).expect("rebind"));
            },
        )
        .await
        .expect_err("changed");
        assert!(
            error
                .to_string()
                .contains("the entry changed after it was probed")
        );
        assert!(tokio::net::UnixStream::connect(&path).await.is_ok());
    }

    #[tokio::test]
    async fn never_removes_a_different_dead_entry_that_replaced_the_probed_one() {
        use crate::socket_ownership::ListenerVerdict;
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("rpc.sock");
        stale_entry(&path);
        let error = prepare_socket_path_with(
            path.to_str().expect("utf8"),
            async |_: &str| pinned(ListenerVerdict::Dead),
            async || {
                std::fs::remove_file(&path).expect("unlink");
                stale_entry(&path);
            },
        )
        .await
        .expect_err("changed");
        assert!(
            error
                .to_string()
                .contains("the entry changed after it was probed")
        );
        assert!(path.symlink_metadata().is_ok());
    }

    #[tokio::test]
    async fn treats_an_entry_another_process_removed_after_the_probe_as_done() {
        use crate::socket_ownership::ListenerVerdict;
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("rpc.sock");
        stale_entry(&path);
        prepare_socket_path_with(
            path.to_str().expect("utf8"),
            async |_: &str| pinned(ListenerVerdict::Dead),
            async || std::fs::remove_file(&path).expect("unlink"),
        )
        .await
        .expect("vanished");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn refuses_a_path_the_kernel_would_truncate_and_listens_privately() {
        use std::os::unix::fs::PermissionsExt;
        let long = format!("/tmp/{}", "x".repeat(120));
        let error = prepare_socket_path(&long).await.expect_err("too long");
        assert!(error.to_string().contains("-byte limit"));
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("rpc.sock");
        let _listener = listen(path.to_str().expect("utf8")).expect("listen");
        assert_eq!(
            std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn a_sealed_host_is_ready_and_a_wrong_key_is_unproven() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("host.sock");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let Ok(stream) = crate::internal_channel::accept_internal_connection(
                        socket,
                        &KEY,
                        Map::new(),
                    )
                    .await
                    else {
                        return;
                    };
                    let (read, mut write) = tokio::io::split(stream);
                    let mut lines = tokio::io::BufReader::new(read).lines();
                    if let Ok(Some(_)) = lines.next_line().await {
                        let _ = write
                            .write_all(b"{\"id\":\"internal-ready\",\"type\":\"response\",\"success\":true,\"data\":{\"instanceId\":\"i\"}}\n")
                            .await;
                        let _ = write.flush().await;
                    }
                });
            }
        });
        assert_eq!(
            probe_internal_listener(&path, &KEY, 5000).await,
            ReadinessProbe::Ready(json!({"instanceId":"i"}))
        );
        assert_eq!(
            probe_internal_listener(&path, &[8; 32], 5000).await,
            ReadinessProbe::Unproven("host_identity_unproven")
        );
        assert_eq!(
            probe_internal_listener(&dir.path().join("absent"), &KEY, 5000).await,
            ReadinessProbe::Refused
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_child_that_accepts_and_never_answers_is_silent() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("host.sock");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let held = tokio::spawn(async move { listener.accept().await.map(|(socket, _)| socket) });
        assert_eq!(
            probe_internal_listener(&path, &KEY, 1000).await,
            ReadinessProbe::Silent
        );
        drop(held);
    }

    #[tokio::test]
    async fn a_relay_cannot_inject_commands_after_authentication() {
        // senpi: a substituted socket relay cannot inject list_sessions after authentication; the hop is lost.
        let (client, host) = tokio::net::UnixStream::pair().expect("pair");
        let server = tokio::spawn(async move {
            crate::internal_channel::accept_internal_connection(host, &KEY, Map::new()).await
        });
        let client = connect_internal_connection(client, &KEY, Some(5000))
            .await
            .expect("client");
        let stream = server.await.expect("join").expect("server");
        let mut raw = client.into_inner_for_test();
        raw.write_all(b"{\"id\":\"forged\",\"type\":\"list_sessions\"}\n")
            .await
            .expect("inject");
        let mut lines = tokio::io::BufReader::new(stream).lines();
        assert!(lines.next_line().await.is_err());
    }
}
