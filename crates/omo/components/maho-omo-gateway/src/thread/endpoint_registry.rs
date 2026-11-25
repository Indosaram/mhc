//! Read-only mirror of senpi's endpoint registry (`thread/endpoint-registry.ts`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EndpointIdentitySource {
    Endpoint,
    Settings,
    GenerationSettings,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryEndpoint {
    pub socket: Option<String>,
    pub dir: PathBuf,
    pub identity: EndpointIdentitySource,
    pub endpoint_kind: EndpointKind,
    pub registry_version: Option<i64>,
}

/// Only this layout writes `endpoint.json`; an agent dir without its marker has no registry.
pub const ENDPOINT_REGISTRY_LAYOUT: i64 = 2;

fn is_endpoint_directory_name(name: &str) -> bool {
    name.len() == 16 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A terminal control socket's name (`t-<sha256(instanceId)[:16]>.sock`).
pub fn is_tui_control_socket(socket: &str) -> bool {
    let Some(base) = Path::new(socket).file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    let Some(rest) = base
        .strip_prefix("t-")
        .and_then(|s| s.strip_suffix(".sock"))
    else {
        return false;
    };
    is_endpoint_directory_name(rest)
}

/// The secret a terminal control endpoint authenticates every connection with (`<socket>.secret`).
pub fn control_socket_secret_path(socket: &str) -> String {
    format!("{socket}.secret")
}

/// Canonicalizes `socket`'s directory through its deepest existing ancestor.
pub fn canonical_endpoint_path(socket: &str) -> String {
    if socket.starts_with('\0') {
        return socket.to_owned();
    }
    let path = Path::new(socket);
    let Some(parent) = path.parent() else {
        return socket.to_owned();
    };
    let Some(file_name) = path.file_name() else {
        return socket.to_owned();
    };
    canonical_directory(parent)
        .join(file_name)
        .to_string_lossy()
        .into_owned()
}

fn canonical_directory(directory: &Path) -> PathBuf {
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut current = directory;
    loop {
        if let Ok(real) = std::fs::canonicalize(current) {
            let mut out = real;
            for part in missing.into_iter().rev() {
                out.push(part);
            }
            return out;
        }
        let Some(parent) = current.parent() else {
            return directory.to_path_buf();
        };
        if parent == current {
            return directory.to_path_buf();
        }
        if let Some(name) = current.file_name() {
            missing.push(name.to_os_string());
        }
        current = parent;
    }
}

fn hash_name(endpoint: &str) -> String {
    let digest = Sha256::digest(endpoint.as_bytes());
    hex::encode(&digest[..8])
}

/// Whether a record naming `socket` belongs in directory `name`.
pub fn socket_names_directory(socket: &str, name: &str) -> bool {
    hash_name(&canonical_endpoint_path(socket)) == name || hash_name(socket) == name
}

fn read_record(file: &Path) -> Option<Map<String, Value>> {
    let text = std::fs::read_to_string(file).ok()?;
    match serde_json::from_str::<Value>(&text).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn record_naming(file: &Path, dir: &Path) -> Option<(String, Map<String, Value>)> {
    let record = read_record(file)?;
    let socket = record
        .get("socket")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_owned();
    let dir_name = dir.file_name()?.to_str()?;
    socket_names_directory(&socket, dir_name).then_some((socket, record))
}

fn identify(dir: &Path) -> RegistryEndpoint {
    if let Some((socket, record)) = record_naming(&dir.join("endpoint.json"), dir) {
        let kind = if record.get("endpoint_kind").and_then(Value::as_str) == Some("tui") {
            EndpointKind::Tui
        } else {
            EndpointKind::RpcHost
        };
        let registry_version = record.get("registry_version").and_then(Value::as_i64);
        return RegistryEndpoint {
            socket: Some(socket),
            dir: dir.to_path_buf(),
            identity: EndpointIdentitySource::Endpoint,
            endpoint_kind: kind,
            registry_version,
        };
    }
    if let Some((socket, _)) = record_naming(&dir.join("settings.json"), dir) {
        return RegistryEndpoint {
            socket: Some(socket),
            dir: dir.to_path_buf(),
            identity: EndpointIdentitySource::Settings,
            endpoint_kind: EndpointKind::RpcHost,
            registry_version: None,
        };
    }
    if let Ok(rd) = std::fs::read_dir(dir.join("generations")) {
        let mut names: Vec<String> = rd
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .collect();
        names.sort();
        for instance_id in names {
            if let Some((socket, _)) = record_naming(
                &dir.join("generations")
                    .join(instance_id)
                    .join("settings.json"),
                dir,
            ) {
                return RegistryEndpoint {
                    socket: Some(socket),
                    dir: dir.to_path_buf(),
                    identity: EndpointIdentitySource::GenerationSettings,
                    endpoint_kind: EndpointKind::RpcHost,
                    registry_version: None,
                };
            }
        }
    }
    RegistryEndpoint {
        socket: None,
        dir: dir.to_path_buf(),
        identity: EndpointIdentitySource::Unknown,
        endpoint_kind: EndpointKind::RpcHost,
        registry_version: None,
    }
}

/// The session files an endpoint directory's `reservations/` still claims.
pub fn claimed_session_paths(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir.join("reservations")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|n| n.ends_with(".json"))
        .collect();
    names.sort();
    let mut paths = Vec::new();
    for name in names {
        if let Some(rec) = read_record(&dir.join("reservations").join(name))
            && let Some(path) = rec
                .get("sessionPath")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        {
            paths.push(path.to_owned());
        }
    }
    paths
}

pub async fn claimed_session_paths_async(dir: &Path) -> Vec<String> {
    let owned = dir.to_path_buf();
    tokio::task::spawn_blocking(move || claimed_session_paths(&owned))
        .await
        .unwrap_or_default()
}

/// Every endpoint directory under `agent_dir`, sorted by directory name; `[]` before layout 2.
pub fn list_registry_endpoints(agent_dir: &Path) -> Vec<RegistryEndpoint> {
    let flat_dir = agent_dir.join("rpc-host-daemon");
    let Some(marker) = read_record(&flat_dir.join("layout.json")) else {
        return Vec::new();
    };
    if marker.get("layout").and_then(Value::as_i64) != Some(ENDPOINT_REGISTRY_LAYOUT) {
        return Vec::new();
    }
    let Ok(rd) = std::fs::read_dir(&flat_dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| {
            e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && e.file_name()
                    .to_str()
                    .is_some_and(is_endpoint_directory_name)
        })
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs.into_iter().map(|d| identify(&d)).collect()
}

pub async fn list_registry_endpoints_async(agent_dir: &Path) -> Vec<RegistryEndpoint> {
    let owned = agent_dir.to_path_buf();
    tokio::task::spawn_blocking(move || list_registry_endpoints(&owned))
        .await
        .unwrap_or_default()
}

/// The kind of `socket` from what the registry and the socket's name say, contacting nothing.
pub fn endpoint_kind_of(socket: &str, registry: &[RegistryEndpoint]) -> EndpointKind {
    if is_tui_control_socket(socket) {
        return EndpointKind::Tui;
    }
    let canonical = canonical_endpoint_path(socket);
    let recorded = registry.iter().find(|entry| {
        entry
            .socket
            .as_deref()
            .is_some_and(|s| s == socket || canonical_endpoint_path(s) == canonical)
    });
    recorded
        .map(|e| e.endpoint_kind)
        .unwrap_or(EndpointKind::RpcHost)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_hash(sock: &str) -> String {
        hash_name(&canonical_endpoint_path(sock))
    }

    #[test]
    fn lists_tui_legacy_settings_and_foreign_records_with_expected_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let flat = dir.path().join("rpc-host-daemon");
        std::fs::create_dir_all(&flat).unwrap();
        std::fs::write(flat.join("layout.json"), r#"{"layout":2}"#).unwrap();

        let tui_sock = "/tmp/t-0123456789abcdef.sock";
        let tui_dir = flat.join(dir_hash(tui_sock));
        std::fs::create_dir_all(&tui_dir).unwrap();
        std::fs::write(
            tui_dir.join("endpoint.json"),
            serde_json::json!({ "socket": tui_sock, "endpoint_kind": "tui", "registry_version": 2 }).to_string(),
        )
        .unwrap();

        let settings_sock = "/tmp/rpc-settings.sock";
        let settings_dir = flat.join(dir_hash(settings_sock));
        std::fs::create_dir_all(&settings_dir).unwrap();
        std::fs::write(
            settings_dir.join("settings.json"),
            serde_json::json!({ "socket": settings_sock }).to_string(),
        )
        .unwrap();

        let foreign_dir = flat.join("ffffffffffffffff");
        std::fs::create_dir_all(&foreign_dir).unwrap();
        std::fs::write(
            foreign_dir.join("endpoint.json"),
            serde_json::json!({ "socket": "/tmp/mismatch.sock" }).to_string(),
        )
        .unwrap();

        let listed = list_registry_endpoints(dir.path());
        assert_eq!(listed.len(), 3);
        let tui = listed
            .iter()
            .find(|e| e.socket.as_deref() == Some(tui_sock))
            .unwrap();
        assert_eq!(tui.endpoint_kind, EndpointKind::Tui);
        assert_eq!(tui.identity, EndpointIdentitySource::Endpoint);
        assert_eq!(tui.registry_version, Some(2));
        let legacy = listed
            .iter()
            .find(|e| e.socket.as_deref() == Some(settings_sock))
            .unwrap();
        assert_eq!(legacy.identity, EndpointIdentitySource::Settings);
        assert_eq!(legacy.endpoint_kind, EndpointKind::RpcHost);
        let foreign = listed.iter().find(|e| e.dir == foreign_dir).unwrap();
        assert_eq!(foreign.identity, EndpointIdentitySource::Unknown);
        assert!(foreign.socket.is_none());
    }

    #[test]
    fn canonicalizes_symlinked_socket_directories() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        let link = dir.path().join("link");
        std::fs::create_dir_all(&real).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let sock_real = real.join("host.sock");
        let sock_link = link.join("host.sock");
        let real_hash = hash_name(&canonical_endpoint_path(&sock_real.to_string_lossy()));
        assert!(socket_names_directory(
            &sock_link.to_string_lossy(),
            &real_hash
        ));
    }

    #[test]
    fn returns_empty_when_layout_marker_is_missing_or_wrong() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_registry_endpoints(dir.path()).is_empty());
        let flat = dir.path().join("rpc-host-daemon");
        std::fs::create_dir_all(&flat).unwrap();
        std::fs::write(flat.join("layout.json"), r#"{"layout":1}"#).unwrap();
        assert!(list_registry_endpoints(dir.path()).is_empty());
    }

    #[test]
    fn endpoint_kind_follows_tui_socket_name_and_registry() {
        assert_eq!(
            endpoint_kind_of("/run/t-0123456789abcdef.sock", &[]),
            EndpointKind::Tui
        );
        assert_eq!(
            endpoint_kind_of("/run/rpc.sock", &[]),
            EndpointKind::RpcHost
        );
        let reg = vec![RegistryEndpoint {
            socket: Some("/run/custom-tui.sock".into()),
            dir: PathBuf::from("/tmp/x"),
            identity: EndpointIdentitySource::Endpoint,
            endpoint_kind: EndpointKind::Tui,
            registry_version: Some(2),
        }];
        assert_eq!(
            endpoint_kind_of("/run/custom-tui.sock", &reg),
            EndpointKind::Tui
        );
    }

    #[test]
    fn claimed_session_paths_reads_valid_json_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let res = dir.path().join("reservations");
        std::fs::create_dir_all(&res).unwrap();
        std::fs::write(res.join("01.json"), r#"{"sessionPath":"/s/1.jsonl"}"#).unwrap();
        std::fs::write(res.join("02.json"), r#"{"sessionPath":""}"#).unwrap();
        std::fs::write(res.join("03.txt"), r#"{"sessionPath":"/s/ignored"}"#).unwrap();
        std::fs::write(res.join("04.json"), r#"{"sessionPath":"/s/2.jsonl"}"#).unwrap();
        assert_eq!(
            claimed_session_paths(dir.path()),
            vec!["/s/1.jsonl".to_owned(), "/s/2.jsonl".to_owned()]
        );
    }

    #[test]
    fn claimed_session_paths_empty_when_no_reservations_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(claimed_session_paths(dir.path()).is_empty());
    }
}
