//! Which endpoints an agent directory holds state for, how a shard is named, and whether an endpoint is
//! routable (senpi `host-endpoints.ts`, `host-endpoint-names.ts` shard half, `host-endpoint-liveness.ts`).
//! Reading only: nothing here writes, unlinks or signals.
use crate::{
    host_daemon_paths::{
        HOST_DAEMON_LAYOUT, HostDaemonPaths, daemon_directory_name, generation_paths,
        host_daemon_directory_paths,
    },
    host_daemon_state::{parse_json, read_file_or_undefined},
};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// What serves an endpoint: a host, or a terminal's control socket (`endpoint.json` `endpoint_kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointKind {
    RpcHost,
    Tui,
}
impl EndpointKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RpcHost => "rpc_host",
            Self::Tui => "tui",
        }
    }
}

/// Owner-keyed shard naming: `p` shards belong to a parent session, `i` shards to an interactive thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShardKind {
    P,
    I,
}
impl ShardKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::P => "p",
            Self::I => "i",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "p" => Some(Self::P),
            "i" => Some(Self::I),
            _ => None,
        }
    }
}
/// `sha256("<kind>:<owner>")` in hex, first 16 characters.
pub fn shard_key(kind: ShardKind, owner: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{}:{owner}", kind.as_str()).as_bytes())
    )[..16]
        .to_owned()
}
/// The socket of a shard whose key is already known: `<root>/<kind>-<key>.sock`.
pub fn shard_socket_path_for_key(root: &Path, kind: ShardKind, key: &str) -> PathBuf {
    root.join(format!("{}-{key}.sock", kind.as_str()))
}
/// Which shard a socket names, from its basename alone.
pub fn parse_shard_socket(socket: &str) -> Option<(ShardKind, String)> {
    let name = Path::new(socket).file_name()?.to_str()?;
    let (kind, rest) = name.split_once('-')?;
    let key = rest.strip_suffix(".sock")?;
    (key.len() == 16
        && key
            .bytes()
            .all(|byte| matches!(byte,b'0'..=b'9'|b'a'..=b'f')))
    .then(|| ShardKind::parse(kind))
    .flatten()
    .map(|kind| (kind, key.to_owned()))
}
/// A terminal control socket is named `t-<16hex>.sock`.
pub fn is_tui_control_socket(socket: &str) -> bool {
    let Some(name) = Path::new(socket).file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.strip_prefix("t-")
        .and_then(|rest| rest.strip_suffix(".sock"))
        .is_some_and(|key| {
            key.len() == 16
                && key
                    .bytes()
                    .all(|byte| matches!(byte,b'0'..=b'9'|b'a'..=b'f'))
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEndpointEntry {
    /// The endpoint this directory serves, or `None` when nothing in it names one.
    pub socket: Option<String>,
    pub dir: PathBuf,
    /// Which file supplied `socket`: `endpoint`, `settings`, `generation-settings` or `unknown`.
    pub identity: &'static str,
    pub endpoint_kind: EndpointKind,
}
fn directory_name(dir: &Path) -> String {
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}
/// The record's socket, accepted only when it hashes to the directory it was found in.
fn record_naming(
    file: &Path,
    dir: &Path,
) -> Option<(String, serde_json::Map<String, serde_json::Value>)> {
    let record = parse_json(read_file_or_undefined(file).ok().flatten().as_deref())?;
    let socket = record
        .get("socket")?
        .as_str()
        .filter(|socket| !socket.is_empty())?
        .to_owned();
    (daemon_directory_name(&socket) == directory_name(dir)).then_some((socket, record))
}
fn layout_is_current(flat_dir: &Path) -> bool {
    parse_json(
        read_file_or_undefined(&flat_dir.join("layout.json"))
            .ok()
            .flatten()
            .as_deref(),
    )
    .and_then(|marker| marker.get("layout").and_then(serde_json::Value::as_u64))
        == Some(u64::from(HOST_DAEMON_LAYOUT))
}
fn is_endpoint_directory_name(name: &str) -> bool {
    name.len() == 16
        && name
            .bytes()
            .all(|byte| matches!(byte,b'0'..=b'9'|b'a'..=b'f'))
}
/// Every endpoint directory under `agent_dir`, sorted by directory name; empty before layout 2.
pub fn list_host_endpoints(agent_dir: &Path) -> Vec<HostEndpointEntry> {
    let flat_dir = agent_dir.join("rpc-host-daemon");
    if !layout_is_current(&flat_dir) {
        return Vec::new();
    }
    let mut dirs = std::fs::read_dir(&flat_dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_dir())
                && is_endpoint_directory_name(&entry.file_name().to_string_lossy())
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    dirs.sort();
    dirs.into_iter()
        .map(|dir| identify_endpoint(&dir))
        .collect()
}
fn identify_endpoint(dir: &Path) -> HostEndpointEntry {
    let paths = host_daemon_directory_paths(dir);
    if let Some((socket, record)) = record_naming(&paths.endpoint_file, dir) {
        return HostEndpointEntry {
            socket: Some(socket),
            dir: dir.into(),
            identity: "endpoint",
            endpoint_kind: endpoint_kind_of(&record),
        };
    }
    if let Some((socket, _)) = record_naming(&paths.settings_file, dir) {
        return HostEndpointEntry {
            socket: Some(socket),
            dir: dir.into(),
            identity: "settings",
            endpoint_kind: EndpointKind::RpcHost,
        };
    }
    let mut generations = std::fs::read_dir(&paths.generations_dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    generations.sort();
    for instance_id in generations {
        if let Some((socket, _)) =
            record_naming(&generation_paths(&paths, &instance_id).settings_file, dir)
        {
            return HostEndpointEntry {
                socket: Some(socket),
                dir: dir.into(),
                identity: "generation-settings",
                endpoint_kind: EndpointKind::RpcHost,
            };
        }
    }
    HostEndpointEntry {
        socket: None,
        dir: dir.into(),
        identity: "unknown",
        endpoint_kind: EndpointKind::RpcHost,
    }
}
fn endpoint_kind_of(record: &serde_json::Map<String, serde_json::Value>) -> EndpointKind {
    if record
        .get("endpoint_kind")
        .and_then(serde_json::Value::as_str)
        == Some("tui")
    {
        EndpointKind::Tui
    } else {
        EndpointKind::RpcHost
    }
}
/// What serves `socket`, from disk and its name alone: `tui` for a terminal control socket's name or an
/// `endpoint.json` that says so. Contacts nothing.
pub fn endpoint_kind_of_socket(socket: &str, agent_dir: &Path) -> EndpointKind {
    if is_tui_control_socket(socket) {
        return EndpointKind::Tui;
    }
    let paths = crate::host_daemon_paths::create_host_daemon_paths(socket, agent_dir);
    record_naming(&paths.endpoint_file, &paths.dir).map_or(EndpointKind::RpcHost, |(_, record)| {
        endpoint_kind_of(&record)
    })
}

/// Whether an endpoint can be talked to right now, and when it provably never will be again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointLiveness {
    Routable,
    LiveUnresponsive,
    Dead,
}
impl EndpointLiveness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Routable => "routable",
            Self::LiveUnresponsive => "live_unresponsive",
            Self::Dead => "dead",
        }
    }
}
/// A TUI answers from the user's terminal: a suspended one must cost a listing at most this long.
pub const TUI_PROBE_TIMEOUT_MS: u64 = 1_500;
pub const DEFAULT_PROBE_TIMEOUT_MS: u64 = 10_000;
pub fn endpoint_probe_timeout_ms(kind: EndpointKind, requested: Option<u64>) -> u64 {
    if kind == EndpointKind::Tui {
        requested
            .unwrap_or(TUI_PROBE_TIMEOUT_MS)
            .min(TUI_PROBE_TIMEOUT_MS)
    } else {
        requested.unwrap_or(DEFAULT_PROBE_TIMEOUT_MS)
    }
}
/// `answered`: `None` when nothing answered, `Some(None)` when a host answered without naming its instance.
pub fn judge_endpoint_liveness(
    paths: &HostDaemonPaths,
    answered: Option<Option<&str>>,
) -> EndpointLiveness {
    if let Some(Some(instance)) = answered
        && records_instance(paths, instance)
    {
        return EndpointLiveness::Routable;
    }
    if answered.is_some() {
        return EndpointLiveness::LiveUnresponsive;
    }
    if any_generation_live(paths) {
        EndpointLiveness::LiveUnresponsive
    } else {
        EndpointLiveness::Dead
    }
}
/// Whether `instance_id` is a generation this directory recorded; matched against listed names, never joined into a path.
fn records_instance(paths: &HostDaemonPaths, instance_id: &str) -> bool {
    let recorded = std::fs::read_dir(&paths.generations_dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy() == instance_id);
    recorded && generation_paths(paths, instance_id).pid_file.exists()
}
/// EVERY generation, not only the pointer's: a predecessor draining after a handoff is still serving.
pub fn any_generation_live(paths: &HostDaemonPaths) -> bool {
    for entry in std::fs::read_dir(&paths.generations_dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
    {
        let generation = generation_paths(paths, &entry.file_name().to_string_lossy());
        if crate::host_stalled_evidence::host_child_alive(&generation, |_| {}) == Some(true) {
            return true;
        }
        if generation_record_is_live(&generation.pid_file) {
            return true;
        }
    }
    false
}
/// A pidfile's process: live and still the recorded process; a live pid whose identity cannot be compared counts as live.
fn generation_record_is_live(pid_file: &Path) -> bool {
    let Some(record) = parse_json(read_file_or_undefined(pid_file).ok().flatten().as_deref())
    else {
        return false;
    };
    let Some(pid) = record
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
    else {
        return false;
    };
    if !crate::host_reservations::process_is_live(pid) {
        return false;
    }
    match (
        record
            .get("processStartTime")
            .and_then(serde_json::Value::as_str),
        crate::host_reservations::read_process_start_time(pid),
    ) {
        (Some(recorded), Some(observed)) => recorded == observed,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shard_names_follow_the_shared_contract() {
        let key = shard_key(ShardKind::P, "owner");
        assert_eq!(key.len(), 16);
        assert_ne!(key, shard_key(ShardKind::I, "owner"));
        let socket = shard_socket_path_for_key(Path::new("/r"), ShardKind::P, &key);
        assert_eq!(socket, PathBuf::from(format!("/r/p-{key}.sock")));
        assert_eq!(
            parse_shard_socket(&socket.to_string_lossy()),
            Some((ShardKind::P, key))
        );
        assert_eq!(parse_shard_socket("/r/other.sock"), None);
        assert!(is_tui_control_socket("/r/t-0123456789abcdef.sock"));
        assert!(!is_tui_control_socket("/r/p-0123456789abcdef.sock"));
    }
    #[test]
    fn endpoints_are_listed_from_disk_and_a_foreign_record_is_not_believed() {
        let temp = tempfile::tempdir().unwrap();
        assert!(list_host_endpoints(temp.path()).is_empty());
        let paths = crate::host_daemon_paths::create_host_daemon_paths("sock", temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();
        std::fs::write(
            &paths.endpoint_file,
            serde_json::json!({"socket":"sock","endpoint_kind":"tui"}).to_string(),
        )
        .unwrap();
        let listed = list_host_endpoints(temp.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(
            (
                listed[0].socket.as_deref(),
                listed[0].identity,
                listed[0].endpoint_kind
            ),
            (Some("sock"), "endpoint", EndpointKind::Tui)
        );
        assert_eq!(
            endpoint_kind_of_socket("sock", temp.path()),
            EndpointKind::Tui
        );
        std::fs::write(
            &paths.endpoint_file,
            serde_json::json!({"socket":"other"}).to_string(),
        )
        .unwrap();
        assert_eq!(list_host_endpoints(temp.path())[0].socket, None);
    }
    #[test]
    fn a_directory_with_no_live_generation_is_dead_and_an_unnamed_answer_is_unresponsive() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::create_host_daemon_paths("sock", temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();
        assert_eq!(
            judge_endpoint_liveness(&paths, None),
            EndpointLiveness::Dead
        );
        assert_eq!(
            judge_endpoint_liveness(&paths, Some(None)),
            EndpointLiveness::LiveUnresponsive
        );
        let generation = generation_paths(&paths, "g1");
        crate::host_daemon_paths::create_generation_directory(&generation).unwrap();
        std::fs::write(
            &generation.pid_file,
            serde_json::json!({"pid":std::process::id(),"processStartTime":null}).to_string(),
        )
        .unwrap();
        assert_eq!(
            judge_endpoint_liveness(&paths, Some(Some("g1"))),
            EndpointLiveness::Routable
        );
        assert_eq!(
            judge_endpoint_liveness(&paths, None),
            EndpointLiveness::LiveUnresponsive
        );
    }
    #[test]
    fn tui_probes_never_exceed_their_budget() {
        assert_eq!(
            endpoint_probe_timeout_ms(EndpointKind::Tui, Some(10_000)),
            TUI_PROBE_TIMEOUT_MS
        );
        assert_eq!(
            endpoint_probe_timeout_ms(EndpointKind::RpcHost, None),
            DEFAULT_PROBE_TIMEOUT_MS
        );
    }
}
