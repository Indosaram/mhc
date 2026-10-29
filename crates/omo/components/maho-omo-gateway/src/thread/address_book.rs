//! The cross-project thread address book.
//!
//! Upstream counterpart: `thread/address-book.ts`. This function retains no process state: host
//! liveness is recomputed solely from this call's host results, and durable disk sessions are the
//! only fallback when a host disappears.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::adapter::{EndpointLiveness, GatewayEndpointRef};
use super::contracts_results::ThreadStatus;
use super::engine::GatewayAddressEntry;
use super::types::GatewayEndpointKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSessionStatus {
    Opening,
    Open,
    Closing,
    Closed,
}

impl HostSessionStatus {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "opening" => Some(Self::Opening),
            "open" => Some(Self::Open),
            "closing" => Some(Self::Closing),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSession {
    pub session_id: String,
    pub durable_session_id: Option<String>,
    pub session_path: Option<String>,
    pub cwd: String,
    pub name: Option<String>,
    pub status: Option<HostSessionStatus>,
    pub kind: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// What a host's `list_sessions` probe answered: the sessions, or the failure it reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostListSessions {
    pub sessions: Vec<HostSession>,
    pub error: Option<String>,
}

impl HostListSessions {
    pub fn ok(sessions: Vec<HostSession>) -> Self {
        Self { sessions, error: None }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self { sessions: Vec::new(), error: Some(error.into()) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressBookHost {
    pub socket: String,
    pub list_sessions: Option<HostListSessions>,
    pub endpoint_kind: Option<GatewayEndpointKind>,
    /// The engine's liveness verdict for the endpoint, when it gave one.
    pub alive: Option<bool>,
    pub reason: Option<String>,
    pub legacy: bool,
}

/// Where a thread runs: a terminal, a Desktop thread shard, a task child, or the shared daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadSurface {
    Tui,
    Desktop,
    Child,
    Daemon,
}

impl ThreadSurface {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Desktop => "desktop",
            Self::Child => "child",
            Self::Daemon => "daemon",
        }
    }
}

/// A session read from disk, for a thread no endpoint lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskSession {
    pub durable_id: String,
    pub name: Option<String>,
    pub cwd: String,
    pub created_at: String,
    pub updated_at: Option<String>,
    pub session_path: String,
    pub source_host: Option<String>,
    pub first_user_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressEndpoint {
    pub kind: GatewayEndpointKind,
    pub socket: String,
    pub routing_id: Option<String>,
    pub durable_session_id: String,
}

/// Cross-project address record; the snake-case summary fields let the addressing layer consume it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressEntry {
    pub durable_id: String,
    pub routing_id: Option<String>,
    pub name: Option<String>,
    pub cwd: String,
    pub status: ThreadStatus,
    pub liveness: ThreadStatus,
    pub source_host: Option<String>,
    pub session_path: Option<String>,
    pub created_at: String,
    pub updated_at: Option<String>,
    pub thread_id: String,
    pub error_note: Option<String>,
    pub endpoint: Option<AddressEndpoint>,
    pub surface: Option<ThreadSurface>,
    pub alive: bool,
    pub title: Option<String>,
}

/// The title a listing shows: the name, else the first user message's opening, never the durable id.
pub fn thread_title(name: Option<&str>, first_user_text: Option<&str>) -> Option<String> {
    if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
        return Some(name.to_owned());
    }
    first_user_text.and_then(|text| {
        let opening: String = text.trim().chars().take(80).collect();
        (!opening.is_empty()).then_some(opening)
    })
}

/// senpi's surface for a session, from what serves it.
pub fn derive_surface(endpoint_kind: Option<GatewayEndpointKind>, session_kind: Option<&str>, socket: &str) -> ThreadSurface {
    if endpoint_kind == Some(GatewayEndpointKind::Tui) {
        return ThreadSurface::Tui;
    }
    if session_kind == Some("worker") {
        return ThreadSurface::Child;
    }
    let name = Path::new(socket).file_name().and_then(|name| name.to_str()).unwrap_or_default();
    if name.starts_with("i-") {
        return ThreadSurface::Desktop;
    }
    if name.starts_with("p-") {
        return ThreadSurface::Child;
    }
    ThreadSurface::Daemon
}

fn host_failure(host: &AddressBookHost) -> Option<String> {
    if host.reason.as_deref() == Some("live_unresponsive") {
        return Some("live_unresponsive".to_owned());
    }
    let result = host.list_sessions.as_ref()?;
    if let Some(error) = &result.error {
        return Some(error.clone());
    }
    None
}

fn from_disk(session: &DiskSession, host: Option<&AddressBookHost>, failure: Option<&str>) -> AddressEntry {
    let endpoint = session.source_host.as_ref().map(|socket| AddressEndpoint {
        kind: host.and_then(|host| host.endpoint_kind).unwrap_or(GatewayEndpointKind::RpcHost),
        socket: socket.clone(),
        routing_id: None,
        durable_session_id: session.durable_id.clone(),
    });
    AddressEntry {
        durable_id: session.durable_id.clone(),
        routing_id: None,
        name: session.name.clone(),
        cwd: session.cwd.clone(),
        status: ThreadStatus::Resumable,
        liveness: ThreadStatus::Resumable,
        source_host: session.source_host.clone(),
        session_path: Some(session.session_path.clone()),
        created_at: session.created_at.clone(),
        updated_at: session.updated_at.clone(),
        thread_id: session.durable_id.clone(),
        error_note: failure.map(str::to_owned),
        surface: endpoint.as_ref().map(|endpoint| derive_surface(Some(endpoint.kind), None, &endpoint.socket)),
        endpoint,
        alive: false,
        title: thread_title(session.name.as_deref(), session.first_user_text.as_deref()),
    }
}

/// Assemble one fresh view from this call's host results and the durable disk sessions.
pub fn assemble_address_book(hosts: &[AddressBookHost], disk_sessions: &[DiskSession], assembled_at: &str) -> Vec<AddressEntry> {
    let mut by_durable_id: BTreeMap<String, AddressEntry> = BTreeMap::new();
    let failures: BTreeMap<String, String> = hosts.iter().filter_map(|host| host_failure(host).map(|failure| (host.socket.clone(), failure))).collect();
    let by_socket: BTreeMap<String, &AddressBookHost> = hosts.iter().map(|host| (host.socket.clone(), host)).collect();

    for session in disk_sessions {
        if session.durable_id.is_empty() {
            continue;
        }
        let failure = session.source_host.as_ref().and_then(|socket| failures.get(socket)).map(String::as_str);
        let host = session.source_host.as_ref().and_then(|socket| by_socket.get(socket)).copied();
        by_durable_id.insert(session.durable_id.clone(), from_disk(session, host, failure));
    }

    for host in hosts {
        let Some(listed) = &host.list_sessions else {
            continue;
        };
        if listed.error.is_some() {
            continue;
        }
        for session in &listed.sessions {
            if session.status == Some(HostSessionStatus::Closed) {
                continue;
            }
            // A terminal endpoint lists its one session by its durable id: there is no routing layer.
            let durable_id = session
                .durable_session_id
                .clone()
                .or_else(|| (host.endpoint_kind == Some(GatewayEndpointKind::Tui)).then(|| session.session_id.clone()));
            let Some(durable_id) = durable_id.filter(|id| !id.is_empty()) else {
                continue;
            };
            let disk = by_durable_id.get(&durable_id);
            let session_path = session.session_path.clone().or_else(|| disk.and_then(|disk| disk.session_path.clone()));
            let created_at = session.created_at.clone().or_else(|| disk.map(|disk| disk.created_at.clone())).unwrap_or_else(|| assembled_at.to_owned());
            let updated_at = session.updated_at.clone().or_else(|| disk.and_then(|disk| disk.updated_at.clone()));
            let name = session.name.clone().or_else(|| disk.and_then(|disk| disk.name.clone()));
            let disk_title = disk.and_then(|disk| disk.title.clone().or_else(|| thread_title(disk.name.as_deref(), None)));
            let kind = host.endpoint_kind.unwrap_or(GatewayEndpointKind::RpcHost);
            by_durable_id.insert(
                durable_id.clone(),
                AddressEntry {
                    durable_id: durable_id.clone(),
                    routing_id: Some(session.session_id.clone()),
                    name: name.clone(),
                    cwd: session.cwd.clone(),
                    status: ThreadStatus::Live,
                    liveness: ThreadStatus::Live,
                    source_host: Some(host.socket.clone()),
                    session_path,
                    created_at,
                    updated_at,
                    thread_id: durable_id.clone(),
                    error_note: None,
                    endpoint: Some(AddressEndpoint { kind, socket: host.socket.clone(), routing_id: Some(session.session_id.clone()), durable_session_id: durable_id }),
                    surface: Some(derive_surface(Some(kind), session.kind.as_deref(), &host.socket)),
                    alive: host.alive != Some(false),
                    title: thread_title(name.as_deref(), None).or_else(|| disk_title.clone()),
                },
            );
        }
    }

    let mut entries: Vec<AddressEntry> = by_durable_id.into_values().collect();
    entries.sort_by(|left, right| match (&left.updated_at, &right.updated_at) {
        (Some(left_updated), Some(right_updated)) if left_updated != right_updated => right_updated.cmp(left_updated),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => left.durable_id.cmp(&right.durable_id),
    });
    entries
}

/// The gateway engine's view: a thread is `routable` only while an endpoint that answered lists it.
pub fn to_gateway_address_entries(entries: &[AddressEntry]) -> Vec<GatewayAddressEntry> {
    entries
        .iter()
        .map(|entry| GatewayAddressEntry {
            thread_id: entry.thread_id.clone(),
            name: entry.name.clone().unwrap_or_default(),
            endpoint: entry.endpoint.as_ref().map(|endpoint| GatewayEndpointRef {
                kind: endpoint.kind,
                socket: endpoint.socket.clone(),
                routing_id: endpoint.routing_id.clone(),
                durable_session_id: endpoint.durable_session_id.clone(),
            }),
            liveness: if entry.alive {
                EndpointLiveness::Routable
            } else if entry.error_note.as_deref() == Some("live_unresponsive") {
                EndpointLiveness::LiveUnresponsive
            } else {
                EndpointLiveness::Dead
            },
        })
        .collect()
}

fn json_str(value: Option<&Value>, key: &str) -> Option<String> {
    value?.get(key)?.as_str().map(str::to_owned)
}

/// One session file's durable identity, or none when the file is unreadable or has no header.
pub fn read_disk_session(path: &Path, source_host: Option<&str>) -> Option<DiskSession> {
    let header = std::fs::read_to_string(path).ok()?.lines().next()?.to_owned();
    let value: Value = serde_json::from_str(&header).ok()?;
    let durable_id = json_str(Some(&value), "session_id").or_else(|| json_str(Some(&value), "sessionId")).or_else(|| json_str(Some(&value), "durable_id"))?;
    let cwd = json_str(Some(&value), "cwd").unwrap_or_default();
    let created_at = json_str(Some(&value), "created_at").or_else(|| json_str(Some(&value), "createdAt")).unwrap_or_default();
    Some(DiskSession {
        durable_id,
        name: json_str(Some(&value), "name"),
        cwd,
        created_at,
        updated_at: json_str(Some(&value), "updated_at").or_else(|| json_str(Some(&value), "updatedAt")),
        session_path: path.to_string_lossy().into_owned(),
        source_host: source_host.map(str::to_owned),
        first_user_text: None,
    })
}

/// senpi's session directory name for a cwd, without the closing `--`.
pub fn session_directory_prefix(root: &str) -> String {
    let stripped = root.trim_start_matches(['/', '\\']).replace(['/', '\\', ':'], "-");
    format!("--{stripped}-")
}

fn project_directories(sessions_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(sessions_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| name.starts_with("--") && name.ends_with("--"))
        .collect()
}

fn session_files(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("jsonl"))
        .collect()
}

/// The session files an address names, for a session no endpoint lists.
pub fn find_disk_sessions(sessions_dir: &Path, address: &str, all_scope: bool, workspace_roots: &[String]) -> Vec<DiskSession> {
    let directories = project_directories(sessions_dir);
    let suffix = format!("_{address}.jsonl");
    let mut by_id: Vec<DiskSession> = Vec::new();
    for directory in &directories {
        for file in session_files(&sessions_dir.join(directory)) {
            if file.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.ends_with(&suffix))
                && let Some(session) = read_disk_session(&file, None)
                && session.durable_id == address
            {
                by_id.push(session);
            }
        }
    }
    if !by_id.is_empty() {
        return by_id;
    }
    let wanted = address.trim().to_lowercase();
    if wanted.is_empty() {
        return Vec::new();
    }
    let prefixes: Vec<String> = if all_scope { Vec::new() } else { workspace_roots.iter().map(|root| session_directory_prefix(root)).collect() };
    directories
        .into_iter()
        .filter(|directory| prefixes.is_empty() || prefixes.iter().any(|prefix| directory.starts_with(prefix)))
        .flat_map(|directory| {
            let wanted = wanted.clone();
            session_files(&sessions_dir.join(&directory)).into_iter().filter_map(move |file| {
                let session = read_disk_session(&file, None)?;
                let name = session.name.as_deref()?;
                (name.trim().to_lowercase() == wanted).then_some(session)
            })
        })
        .collect()
}

/// Scan the `sessions/--<encoded-cwd>--/*.jsonl` layout; cwd and durable identity come from the header.
pub fn scan_disk_sessions(sessions_dir: &Path, source_host: Option<&str>) -> Vec<DiskSession> {
    let mut found: Vec<DiskSession> = project_directories(sessions_dir)
        .into_iter()
        .flat_map(|directory| session_files(&sessions_dir.join(directory)))
        .filter_map(|file| read_disk_session(&file, source_host))
        .collect();
    found.sort_by(|left, right| match (&left.updated_at, &right.updated_at) {
        (Some(left_updated), Some(right_updated)) if left_updated != right_updated => right_updated.cmp(left_updated),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => left.durable_id.cmp(&right.durable_id),
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(durable_id: &str) -> DiskSession {
        DiskSession {
            durable_id: durable_id.into(),
            name: Some("Receiver".into()),
            cwd: "/work".into(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: Some("2026-01-02T00:00:00.000Z".into()),
            session_path: format!("/sessions/{durable_id}.jsonl"),
            source_host: Some("/run/i-host.sock".into()),
            first_user_text: None,
        }
    }

    fn host(sessions: Vec<HostSession>) -> AddressBookHost {
        AddressBookHost {
            socket: "/run/i-host.sock".into(),
            list_sessions: Some(HostListSessions::ok(sessions)),
            endpoint_kind: Some(GatewayEndpointKind::RpcHost),
            alive: Some(true),
            reason: None,
            legacy: false,
        }
    }

    fn live_session() -> HostSession {
        HostSession {
            session_id: "r-1".into(),
            durable_session_id: Some("d-1".into()),
            session_path: Some("/sessions/d-1.jsonl".into()),
            cwd: "/work".into(),
            name: Some("Receiver".into()),
            status: Some(HostSessionStatus::Open),
            kind: Some("interactive".into()),
            created_at: Some("2026-01-01T00:00:00.000Z".into()),
            updated_at: Some("2026-01-03T00:00:00.000Z".into()),
        }
    }

    #[test]
    fn a_live_host_listing_turns_a_disk_session_into_a_live_routable_entry() {
        let entries = assemble_address_book(&[host(vec![live_session()])], &[disk("d-1")], "2026-01-04T00:00:00.000Z");
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.status, ThreadStatus::Live);
        assert_eq!(entry.routing_id.as_deref(), Some("r-1"));
        assert_eq!(entry.surface, Some(ThreadSurface::Desktop));
        assert!(entry.alive);
        assert_eq!(to_gateway_address_entries(&entries)[0].liveness, EndpointLiveness::Routable);
    }

    #[test]
    fn a_dead_host_falls_back_to_the_disk_session_with_its_error_note() {
        let mut dead = host(vec![]);
        dead.list_sessions = Some(HostListSessions::failed("connection refused"));
        let entries = assemble_address_book(&[dead], &[disk("d-1")], "2026-01-04T00:00:00.000Z");
        assert_eq!(entries[0].status, ThreadStatus::Resumable);
        assert_eq!(entries[0].error_note.as_deref(), Some("connection refused"));
        assert!(!entries[0].alive);
        assert_eq!(to_gateway_address_entries(&entries)[0].liveness, EndpointLiveness::Dead);
    }

    #[test]
    fn a_live_unresponsive_endpoint_stays_unresponsive_never_gone() {
        let mut unresponsive = host(vec![]);
        unresponsive.reason = Some("live_unresponsive".into());
        unresponsive.alive = Some(false);
        let entries = assemble_address_book(&[unresponsive], &[disk("d-1")], "2026-01-04T00:00:00.000Z");
        assert_eq!(to_gateway_address_entries(&entries)[0].liveness, EndpointLiveness::LiveUnresponsive);
    }

    #[test]
    fn the_surface_follows_what_serves_the_session() {
        assert_eq!(derive_surface(Some(GatewayEndpointKind::Tui), None, "/run/tui-1.sock"), ThreadSurface::Tui);
        assert_eq!(derive_surface(Some(GatewayEndpointKind::RpcHost), Some("worker"), "/run/x.sock"), ThreadSurface::Child);
        assert_eq!(derive_surface(Some(GatewayEndpointKind::RpcHost), None, "/run/i-7.sock"), ThreadSurface::Desktop);
        assert_eq!(derive_surface(Some(GatewayEndpointKind::RpcHost), None, "/run/p-7.sock"), ThreadSurface::Child);
        assert_eq!(derive_surface(Some(GatewayEndpointKind::RpcHost), None, "/run/host.sock"), ThreadSurface::Daemon);
    }

    #[test]
    fn a_title_never_falls_back_to_the_durable_id() {
        assert_eq!(thread_title(Some("Build"), Some("first words")), Some("Build".to_owned()));
        assert_eq!(thread_title(None, Some("first words")), Some("first words".to_owned()));
        assert_eq!(thread_title(None, None), None);
    }

    #[test]
    fn a_session_directory_prefix_encodes_the_workspace() {
        assert_eq!(session_directory_prefix("/home/indo/work"), "--home-indo-work-");
    }
}
