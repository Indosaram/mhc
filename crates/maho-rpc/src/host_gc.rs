//! `host gc`: reclaims the endpoint directories of hosts that are PROVABLY gone, and nothing else (senpi `host-gc.ts`).
//!
//! An endpoint is removed only on three-part evidence evaluated inside its ensure lock: no generation pidfile
//! names a live process, no session-path claim has a live owner, and the socket (and every `.next-*` successor
//! bind) refuses the connection or does not exist. Anything else is kept with the reason. It never signals a
//! process; the stale-older-engine stop (`staleEngine`) is not ported because the engine-staleness judge is absent.
use crate::host_endpoints::{EndpointKind, any_generation_live, list_host_endpoints};
use std::path::{Path, PathBuf};

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
#[derive(Debug, Default, PartialEq, Eq)]
pub struct HostGcResult {
    pub removed: Vec<HostGcEntry>,
    pub kept: Vec<HostGcEntry>,
}
impl HostGcResult {
    /// The wire body: `{removed:[..],kept:[..]}`; `skipped` only on a removal that left a directory, `error` only on `failed`.
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
        serde_json::json!({"removed":self.removed.iter().map(row).collect::<Vec<_>>(),"kept":self.kept.iter().map(row).collect::<Vec<_>>()})
    }
}
#[derive(Debug, Default, Clone)]
pub struct HostGcOptions {
    /// Only endpoints of these kinds; every kind when `None`.
    pub kinds: Option<Vec<EndpointKind>>,
}
enum Judged {
    Removed {
        reason: &'static str,
        skipped: Vec<PathBuf>,
    },
    Kept(&'static str),
}

pub async fn gc_host_endpoints(agent_dir: &Path, options: &HostGcOptions) -> HostGcResult {
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
        match gc_endpoint(socket, &endpoint.dir).await {
            Ok(Judged::Removed { reason, skipped }) => {
                result.removed.push(entry(reason, skipped, None))
            }
            Ok(Judged::Kept(reason)) => result.kept.push(entry(reason, Vec::new(), None)),
            Err(error) => result
                .kept
                .push(entry("failed", Vec::new(), Some(error.to_string()))),
        }
    }
    result
}
/// One endpoint, judged and removed exactly as a full run does.
async fn gc_endpoint(socket: &str, dir: &Path) -> std::io::Result<Judged> {
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
    let judged = judge_and_remove(socket, dir, &paths).await;
    // A removal deletes the directory holding the lock file; a release failure matters only when the endpoint stays.
    let released = lock.release();
    match (&judged, released) {
        (Ok(Judged::Removed { .. }), _) => judged,
        (_, Err(error)) => Err(std::io::Error::other(error)),
        _ => judged,
    }
}
async fn judge_and_remove(
    socket: &str,
    dir: &Path,
    paths: &crate::host_daemon_paths::HostDaemonPaths,
) -> std::io::Result<Judged> {
    if any_generation_live(paths) {
        return Ok(Judged::Kept("live_generation"));
    }
    if crate::host_reservations::read_session_path_claims(&paths.reservations_dir)
        .iter()
        .any(|claim| crate::host_reservations::claim_owner_is_live(&claim.owner))
    {
        return Ok(Judged::Kept("live_claim"));
    }
    let Some(silence) = socket_silence(socket).await else {
        return Ok(Judged::Kept("reachable"));
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
            return Ok(Judged::Kept("reachable"));
        }
    }
    let skipped = remove_endpoint(socket, dir)?;
    Ok(Judged::Removed {
        reason: silence,
        skipped,
    })
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
            },
        )
        .await;
        assert_eq!(result, HostGcResult::default());
        assert!(paths.dir.exists());
    }
}
