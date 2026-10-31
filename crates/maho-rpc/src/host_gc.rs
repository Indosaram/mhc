//! `host gc`: reclaims the endpoint directories of hosts that are PROVABLY gone, and nothing else (senpi `host-gc.ts`).
//!
//! An endpoint is removed only on three-part evidence evaluated inside its ensure lock: no generation pidfile
//! names a live process, no session-path claim has a live owner, and the socket (and every `.next-*` successor
//! bind) refuses the connection or does not exist. Anything else is kept with the reason.
//!
//! `stale_engine` adds the one case where gc ends a RUNNING host (senpi #3268): an endpoint kept as
//! `live_generation` or `reachable` is judged by `host_stale_engine` inside the same lock, and a stale
//! older-engine host is stopped through `stop_host_with` and reported under `stopped`. Its directory is
//! collected by a later run, once the three-part evidence holds. Without the option nothing running is
//! ever judged, and a judged host that is not stale is kept with the `stale_*` reason that saved it.
use crate::host_endpoints::{EndpointKind, any_generation_live, list_host_endpoints};
use crate::host_stale_engine::{
    StaleEngineKeep, StaleEngineOptions, StaleEngineVerdict, judge_stale_engine,
};
use maho_core::engine_build_identity::{EngineBuildIdentity, engine_build_identity};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The budget a concurrent ensure's critical section gets before gc reports `locked`.
pub const GC_LOCK_WAIT_MS: u64 = 2_000;
/// A connect that neither succeeds nor fails within this budget counts as an answer.
pub const SOCKET_PROBE_TIMEOUT_MS: u64 = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostGcEntry {
    pub socket: Option<String>,
    pub dir: PathBuf,
    pub reason: String,
    pub skipped: Vec<PathBuf>,
    pub error: Option<String>,
}
/// A running stale older-engine host this run stopped (`reason: "stale_old_engine"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostGcStopped {
    pub entry: HostGcEntry,
    pub pid: u32,
}
#[derive(Debug, Default, PartialEq, Eq)]
pub struct HostGcResult {
    pub removed: Vec<HostGcEntry>,
    /// Non-empty only when a run stopped a stale older-engine host (`stale_engine`).
    pub stopped: Vec<HostGcStopped>,
    pub kept: Vec<HostGcEntry>,
}
impl HostGcResult {
    /// The wire body: `{removed:[..],kept:[..]}` plus `stopped:[..]` only when a host was stopped; `skipped` only on
    /// a removal that left a directory, `error` only on `failed`.
    pub fn to_value(&self) -> serde_json::Value {
        let row = |entry: &HostGcEntry| {
            let mut value =
                serde_json::json!({"socket":entry.socket,"dir":entry.dir,"reason":entry.reason});
            if !entry.skipped.is_empty() {
                value["skipped"] = entry
                    .skipped
                    .iter()
                    .map(|path| serde_json::json!({"path":path,"type":"directory"}))
                    .collect();
            }
            if let Some(error) = &entry.error {
                value["error"] = error.clone().into();
            }
            value
        };
        let mut value = serde_json::json!({"removed":self.removed.iter().map(row).collect::<Vec<_>>(),"kept":self.kept.iter().map(row).collect::<Vec<_>>()});
        if !self.stopped.is_empty() {
            value["stopped"] = self
                .stopped
                .iter()
                .map(|stopped| {
                    let mut value = row(&stopped.entry);
                    value["pid"] = stopped.pid.into();
                    value
                })
                .collect();
        }
        value
    }
}
/// senpi `StaleEngineOptions`: whom a running host is judged against (this build when `client` is `None`), the
/// clock and the grace past the host's own idle window.
#[derive(Clone, Default)]
pub struct StaleEngineRequest {
    pub client: Option<EngineBuildIdentity>,
    pub now: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    pub grace_ms: Option<f64>,
}
impl std::fmt::Debug for StaleEngineRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StaleEngineRequest")
            .field("client", &self.client)
            .field("now", &self.now.as_ref().map(|_| "<clock>"))
            .field("grace_ms", &self.grace_ms)
            .finish()
    }
}
impl StaleEngineRequest {
    /// `judgeStaleEngine(socket, agentDir, options.client ?? engineBuildIdentity(), options)`.
    pub async fn judge(&self, socket: &str, agent_dir: &Path) -> StaleEngineVerdict {
        let client = self
            .client
            .clone()
            .unwrap_or_else(|| engine_build_identity().clone());
        let now = self.now.clone().map(|now| {
            let boxed: Box<dyn Fn() -> f64 + Send + Sync> = Box::new(move || now());
            boxed
        });
        judge_stale_engine(
            socket,
            agent_dir,
            &client,
            StaleEngineOptions {
                now,
                grace_ms: self.grace_ms,
            },
        )
        .await
    }
}
/// The wire name of a keep reason (`stale_*`).
pub fn stale_keep_reason(keep: StaleEngineKeep) -> &'static str {
    match keep {
        StaleEngineKeep::StaleUnprovenOwner => "stale_unproven_owner",
        StaleEngineKeep::StaleUnreachable => "stale_unreachable",
        StaleEngineKeep::StaleNotOlder => "stale_not_older",
        StaleEngineKeep::StalePersistent => "stale_persistent",
        StaleEngineKeep::StaleSessions => "stale_sessions",
        StaleEngineKeep::StaleClaims => "stale_claims",
        StaleEngineKeep::StaleYoung => "stale_young",
        StaleEngineKeep::StaleClients => "stale_clients",
    }
}
#[derive(Debug, Default, Clone)]
pub struct HostGcOptions {
    /// Only endpoints of these kinds; every kind when `None`.
    pub kinds: Option<Vec<EndpointKind>>,
    /// Also stop running stale older-engine hosts; `Some(Default::default())` judges against this build.
    pub stale_engine: Option<StaleEngineRequest>,
}
/// One endpoint's verdict (senpi `gcEndpoint`'s outcome).
pub enum Judged {
    Removed {
        reason: &'static str,
        skipped: Vec<PathBuf>,
    },
    Kept(&'static str),
    Stopped {
        pid: u32,
    },
}
/// The stop's own reads, kept short so the endpoint's ensure lock is not held for their default budget each.
const STALE_STOP_TIMEOUT_MS: u64 = 2_000;

pub async fn gc_host_endpoints(agent_dir: &Path, options: &HostGcOptions) -> HostGcResult {
    // Dead launches' public key records go first (senpi `gcRpcPublicKeys`); a failure never blocks endpoint gc.
    #[cfg(unix)]
    {
        let dir = agent_dir.to_path_buf();
        let keys = tokio::task::spawn_blocking(move || {
            crate::rpc_public_key::gc_rpc_public_keys(&dir, Default::default())
        })
        .await;
        if let Ok(Err(error)) = keys {
            eprintln!("senpi rpc host gc: key records: {error}");
        }
    }
    let mut result = HostGcResult::default();
    let wanted = |kind: EndpointKind| {
        options
            .kinds
            .as_ref()
            .is_none_or(|kinds| kinds.contains(&kind))
    };
    if wanted(EndpointKind::RpcHost)
        && let Some(legacy) = legacy_flat_directory(agent_dir)
    {
        result.kept.push(HostGcEntry {
            socket: None,
            dir: legacy,
            reason: "legacy_layout".into(),
            skipped: Vec::new(),
            error: None,
        });
    }
    for endpoint in list_host_endpoints(agent_dir) {
        if !wanted(endpoint.endpoint_kind) {
            continue;
        }
        let entry = |reason: &str, skipped: Vec<PathBuf>, error: Option<String>| HostGcEntry {
            socket: endpoint.socket.clone(),
            dir: endpoint.dir.clone(),
            reason: reason.into(),
            skipped,
            error,
        };
        let Some(socket) = &endpoint.socket else {
            result
                .kept
                .push(entry("unknown_identity", Vec::new(), None));
            continue;
        };
        match gc_endpoint(socket, &endpoint.dir, options.stale_engine.as_ref()).await {
            Ok(Judged::Removed { reason, skipped }) => {
                result.removed.push(entry(reason, skipped, None))
            }
            Ok(Judged::Stopped { pid }) => result.stopped.push(HostGcStopped {
                entry: entry("stale_old_engine", Vec::new(), None),
                pid,
            }),
            Ok(Judged::Kept(reason)) => result.kept.push(entry(reason, Vec::new(), None)),
            Err(error) => result
                .kept
                .push(entry("failed", Vec::new(), Some(error.to_string()))),
        }
    }
    result
}
/// One endpoint, judged and removed exactly as a full run does.
pub async fn gc_endpoint(
    socket: &str,
    dir: &Path,
    stale_engine: Option<&StaleEngineRequest>,
) -> std::io::Result<Judged> {
    let paths = crate::host_daemon_paths::host_daemon_directory_paths(dir);
    let retries = crate::ownership_safe_lock::LockRetries {
        retries: u32::try_from(GC_LOCK_WAIT_MS / 100).unwrap_or(20),
        ..Default::default()
    };
    let Ok(mut lock) =
        crate::ownership_safe_lock::acquire_ownership_safe_lock(&paths.lock_file, retries).await
    else {
        return Ok(Judged::Kept("locked"));
    };
    let judged = judge_and_remove(socket, dir, &paths, stale_engine).await;
    // A removal deletes the directory holding the lock file; a release failure matters only when the endpoint stays.
    let released = lock.release();
    match (&judged, released) {
        (Ok(Judged::Removed { .. }), _) => judged,
        (_, Err(error)) => Err(std::io::Error::other(error)),
        _ => judged,
    }
}
/// senpi `endpointInUse`: why an endpoint is still in use, or how its socket proved silent.
enum EndpointEvidence {
    InUse(&'static str),
    Silent(&'static str),
}
async fn endpoint_in_use(
    socket: &str,
    paths: &crate::host_daemon_paths::HostDaemonPaths,
) -> EndpointEvidence {
    if any_generation_live(paths) {
        return EndpointEvidence::InUse("live_generation");
    }
    if crate::host_reservations::read_session_path_claims(&paths.reservations_dir)
        .iter()
        .any(|claim| crate::host_reservations::claim_owner_is_live(&claim.owner))
    {
        return EndpointEvidence::InUse("live_claim");
    }
    let Some(silence) = socket_silence(socket).await else {
        return EndpointEvidence::InUse("reachable");
    };
    for successor in socket_siblings(socket)
        .into_iter()
        .filter(|name| name.starts_with(&format!("{}.next-", base_name(socket))))
    {
        if socket_silence(
            &Path::new(socket)
                .with_file_name(successor)
                .to_string_lossy(),
        )
        .await
        .is_none()
        {
            return EndpointEvidence::InUse("reachable");
        }
    }
    EndpointEvidence::Silent(silence)
}
async fn judge_and_remove(
    socket: &str,
    dir: &Path,
    paths: &crate::host_daemon_paths::HostDaemonPaths,
    stale_engine: Option<&StaleEngineRequest>,
) -> std::io::Result<Judged> {
    match endpoint_in_use(socket, paths).await {
        EndpointEvidence::InUse(in_use) => match stale_engine {
            Some(request) if in_use != "live_claim" => {
                stop_if_stale(socket, dir, in_use, request).await
            }
            _ => Ok(Judged::Kept(in_use)),
        },
        EndpointEvidence::Silent(silence) => {
            let skipped = remove_endpoint(socket, dir)?;
            Ok(Judged::Removed {
                reason: silence,
                skipped,
            })
        }
    }
}
/// senpi `stopIfStale`: a host that is no candidate at all (most run this very engine) keeps the evidence reason it
/// always had; a stale one is stopped through a stop that requires both reads to answer.
async fn stop_if_stale(
    socket: &str,
    dir: &Path,
    in_use: &'static str,
    request: &StaleEngineRequest,
) -> std::io::Result<Judged> {
    // `<agent dir>/rpc-host-daemon/<name>`.
    let agent_dir = dir.parent().and_then(Path::parent).unwrap_or(Path::new(""));
    match request.judge(socket, agent_dir).await {
        StaleEngineVerdict::Keep(
            StaleEngineKeep::StaleUnprovenOwner
            | StaleEngineKeep::StaleUnreachable
            | StaleEngineKeep::StaleNotOlder,
        ) => Ok(Judged::Kept(in_use)),
        StaleEngineVerdict::Keep(keep) => Ok(Judged::Kept(stale_keep_reason(keep))),
        StaleEngineVerdict::Stale { .. } => {
            let outcome = crate::host_stop::stop_host_with(crate::host_stop::StopHostOptions {
                socket: socket.into(),
                agent_dir: Some(agent_dir.into()),
                timeout_ms: Some(STALE_STOP_TIMEOUT_MS),
                require_answer: true,
                ..Default::default()
            })
            .await?;
            Ok(match outcome {
                crate::host_stop::StopHostOutcome::Stopped { pid } => Judged::Stopped { pid },
                crate::host_stop::StopHostOutcome::Refuse {
                    reason: "unknown_owner",
                } => Judged::Kept("stale_unproven_owner"),
                crate::host_stop::StopHostOutcome::Refuse { .. }
                | crate::host_stop::StopHostOutcome::Drain(_) => Judged::Kept("stale_sessions"),
            })
        }
    }
}
fn base_name(socket: &str) -> String {
    Path::new(socket)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}
/// Every entry beside `socket` that belongs to it: `<name>.next-*` successor binds and `<name>.shield-*`.
pub fn socket_siblings(socket: &str) -> Vec<String> {
    let name = base_name(socket);
    let Some(parent) = Path::new(socket).parent() else {
        return Vec::new();
    };
    std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|entry| {
            entry.starts_with(&format!("{name}.next-"))
                || entry.starts_with(&format!("{name}.shield-"))
        })
        .collect()
}
/// How the socket proved silent, or `None` when it did not: only a refused connect to a real socket entry and an
/// absent entry prove nobody listens; a connect that succeeds, times out or fails any other way is an answer.
#[cfg(unix)]
async fn socket_silence(socket: &str) -> Option<&'static str> {
    use std::os::unix::fs::FileTypeExt;
    let attempt = tokio::time::timeout(
        std::time::Duration::from_millis(SOCKET_PROBE_TIMEOUT_MS),
        tokio::net::UnixStream::connect(socket),
    )
    .await;
    match attempt {
        Err(_) | Ok(Ok(_)) => None,
        Ok(Err(error)) => match error.kind() {
            std::io::ErrorKind::NotFound => Some("socket_absent"),
            std::io::ErrorKind::ConnectionRefused => match std::fs::symlink_metadata(socket) {
                Ok(metadata) => metadata.file_type().is_socket().then_some("socket_refused"),
                Err(error) => {
                    (error.kind() == std::io::ErrorKind::NotFound).then_some("socket_absent")
                }
            },
            _ => None,
        },
    }
}
#[cfg(not(unix))]
async fn socket_silence(_socket: &str) -> Option<&'static str> {
    None
}
/// Siblings first (a directory is not a socket's leftover and stays, reported), then the socket, the endpoint directory LAST.
fn remove_endpoint(socket: &str, dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut skipped = Vec::new();
    let parent = Path::new(socket).parent().unwrap_or(Path::new(""));
    for sibling in socket_siblings(socket) {
        let path = parent.join(sibling);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() => skipped.push(path),
            Ok(_) => std::fs::remove_file(&path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    match std::fs::remove_file(socket) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::remove_dir_all(dir)?;
    Ok(skipped)
}
/// The flat daemon directory when it holds state but no layout-2 marker: a legacy host's, never touched.
fn legacy_flat_directory(agent_dir: &Path) -> Option<PathBuf> {
    let flat = agent_dir.join("rpc-host-daemon");
    let marker = crate::host_daemon_state::parse_json(
        crate::host_daemon_state::read_file_or_undefined(&flat.join("layout.json"))
            .ok()
            .flatten()
            .as_deref(),
    );
    if marker.and_then(|marker| marker.get("layout").and_then(serde_json::Value::as_u64))
        == Some(u64::from(crate::host_daemon_paths::HOST_DAEMON_LAYOUT))
    {
        return None;
    }
    flat.join("host.pid").exists().then_some(flat)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn endpoint(temp: &Path, socket: &str) -> crate::host_daemon_paths::HostDaemonPaths {
        let paths = crate::host_daemon_paths::create_host_daemon_paths(socket, temp);
        crate::host_daemon_paths::create_daemon_directories(&paths).expect("daemon directories");
        std::fs::write(
            &paths.endpoint_file,
            serde_json::json!({"socket":socket}).to_string(),
        )
        .expect("endpoint file");
        paths
    }
    #[tokio::test]
    async fn a_silent_endpoint_with_no_live_generation_is_removed_directory_last() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("gone.sock").to_string_lossy().into_owned();
        let paths = endpoint(temp.path(), &socket);
        let result = gc_host_endpoints(temp.path(), &HostGcOptions::default()).await;
        assert_eq!(result.kept, vec![]);
        assert_eq!(result.removed.len(), 1);
        assert_eq!(result.removed[0].reason, "socket_absent");
        assert!(!paths.dir.exists());
    }
    #[tokio::test]
    async fn a_listening_socket_keeps_its_endpoint_as_reachable() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("live.sock");
        let _listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let paths = endpoint(temp.path(), &socket.to_string_lossy());
        let result = gc_host_endpoints(temp.path(), &HostGcOptions::default()).await;
        assert!(result.removed.is_empty());
        assert_eq!(result.kept[0].reason, "reachable");
        assert!(paths.dir.exists());
    }
    #[tokio::test]
    async fn a_live_generation_is_kept_and_an_unnamed_directory_is_unknown_identity() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("busy.sock").to_string_lossy().into_owned();
        let paths = endpoint(temp.path(), &socket);
        let generation = crate::host_daemon_paths::generation_paths(&paths, "g");
        crate::host_daemon_paths::create_generation_directory(&generation).unwrap();
        std::fs::write(
            &generation.pid_file,
            serde_json::json!({"pid":std::process::id(),"processStartTime":null}).to_string(),
        )
        .unwrap();
        let unnamed = temp.path().join("rpc-host-daemon").join("0123456789abcdef");
        std::fs::create_dir_all(&unnamed).unwrap();
        let result = gc_host_endpoints(temp.path(), &HostGcOptions::default()).await;
        let reasons = result
            .kept
            .iter()
            .map(|entry| entry.reason.as_str())
            .collect::<Vec<_>>();
        assert!(
            reasons.contains(&"live_generation") && reasons.contains(&"unknown_identity"),
            "{reasons:?}"
        );
        assert!(unnamed.exists() && paths.dir.exists());
    }
    #[tokio::test]
    async fn kinds_narrow_a_run_and_other_endpoints_are_not_judged() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("gone.sock").to_string_lossy().into_owned();
        let paths = endpoint(temp.path(), &socket);
        let result = gc_host_endpoints(
            temp.path(),
            &HostGcOptions {
                kinds: Some(vec![EndpointKind::Tui]),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result, HostGcResult::default());
        assert!(paths.dir.exists());
    }

    /// A fake older-engine host: the listener is inherited by a `sleep` child that owns the registration, so
    /// the public-client count reads the child's descriptors (the listener alone) and a stop signals the child,
    /// never this test process. This process serves the socket.
    #[cfg(target_os = "linux")]
    struct FakeHost {
        child: std::process::Child,
        socket: String,
        paths: crate::host_daemon_paths::HostDaemonPaths,
    }
    #[cfg(target_os = "linux")]
    impl Drop for FakeHost {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
    #[cfg(target_os = "linux")]
    async fn fake_host(temp: &Path) -> FakeHost {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let socket = temp.join("h.sock").to_string_lossy().into_owned();
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        rustix::io::fcntl_setfd(&listener, rustix::io::FdFlags::empty()).expect("inheritable");
        let child = std::process::Command::new("sleep")
            .arg("1000")
            .spawn()
            .expect("sleep");
        rustix::io::fcntl_setfd(&listener, rustix::io::FdFlags::CLOEXEC).expect("cloexec");
        listener.set_nonblocking(true).expect("nonblocking");
        let listener = tokio::net::UnixListener::from_std(listener).expect("tokio listener");
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut lines = tokio::io::BufReader::new(read).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let request: serde_json::Value = serde_json::from_str(&line).expect("json");
                        let data = match request["type"].as_str() {
                            Some("get_protocol_info") => {
                                serde_json::json!({"serverVersion":"old","capabilities":[],"instanceId":"g1","engineVersion":"2000.1.1","engineOrdinal":[2000,1,1,0,0]})
                            }
                            _ => serde_json::json!({"sessions":[]}),
                        };
                        let reply = serde_json::json!({"id":request["id"],"type":"response","success":true,"data":data});
                        if write
                            .write_all(format!("{reply}\n").as_bytes())
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
        let paths = endpoint(temp, &socket);
        crate::host_daemon_registration::write_host_registration(
            &paths,
            &crate::host_daemon_registration::HostRegistration {
                pid: child.id(),
                process_start_time: crate::host_reservations::read_process_start_time(child.id()),
                socket: socket.clone(),
                instance_id: "g1".into(),
                generation: 1.0,
                launch_profile_id: "profile".into(),
            },
        )
        .expect("registration");
        FakeHost {
            child,
            socket,
            paths,
        }
    }
    #[cfg(target_os = "linux")]
    fn judged_against(version: &str, grace_ms: f64) -> StaleEngineRequest {
        StaleEngineRequest {
            client: Some(
                maho_core::engine_build_identity::engine_build_identity_from(
                    &maho_core::engine_build_identity::EngineBuildInput {
                        version: version.into(),
                        ..Default::default()
                    },
                ),
            ),
            now: Some(Arc::new(|| 1.0e15)),
            grace_ms: Some(grace_ms),
        }
    }
    /// senpi #3268: an older-engine host with no session, reservation or client past its idle window is reported
    /// `stale_old_engine` by `status --all` and stopped by `gc`.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_stale_older_engine_host_is_reported_and_stopped() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut host = fake_host(temp.path()).await;
        let request = judged_against("2999.1.1", 0.0);
        let rows = crate::host_status_all::read_all_host_status_with(
            temp.path(),
            true,
            Some(2_000),
            &request,
        )
        .await
        .expect("status");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["alive"], true, "{}", rows[0]);
        assert_eq!(rows[0]["stale_old_engine"], true);
        let result = gc_host_endpoints(
            temp.path(),
            &HostGcOptions {
                stale_engine: Some(request),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result.kept, vec![], "{:?}", result.kept);
        assert_eq!(result.stopped.len(), 1);
        assert_eq!(result.stopped[0].pid, host.child.id());
        assert_eq!(result.stopped[0].entry.reason, "stale_old_engine");
        assert_eq!(result.to_value()["stopped"][0]["pid"], host.child.id());
        let status = host.child.wait().expect("wait");
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGTERM));
        assert!(
            host.paths.dir.exists(),
            "a stop never removes the directory"
        );
    }
    /// The same engine is no candidate (kept with its evidence reason); a host inside its idle window plus the
    /// grace is kept as `stale_young`; without the option nothing running is judged.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn hosts_that_are_not_stale_keep_their_reason() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut host = fake_host(temp.path()).await;
        for (request, reason) in [
            (judged_against("2000.1.1", 0.0), "live_generation"),
            (judged_against("2999.1.1", 1.0e16), "stale_young"),
        ] {
            let rows = crate::host_status_all::read_all_host_status_with(
                temp.path(),
                true,
                Some(2_000),
                &request,
            )
            .await
            .expect("status");
            assert_eq!(rows[0]["stale_old_engine"], false);
            let result = gc_host_endpoints(
                temp.path(),
                &HostGcOptions {
                    stale_engine: Some(request),
                    ..Default::default()
                },
            )
            .await;
            assert!(result.stopped.is_empty());
            assert_eq!(result.kept.len(), 1);
            assert_eq!(result.kept[0].reason, reason);
            assert!(result.to_value().get("stopped").is_none());
        }
        let plain = gc_host_endpoints(temp.path(), &HostGcOptions::default()).await;
        assert_eq!(plain.kept[0].reason, "live_generation");
        assert!(
            host.child.try_wait().expect("try_wait").is_none(),
            "the host still runs"
        );
        assert_eq!(host.socket, plain.kept[0].socket.clone().expect("socket"));
    }
}
