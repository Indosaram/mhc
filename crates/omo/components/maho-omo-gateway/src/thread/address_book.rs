//! Local address book merging live endpoint session lists with disk session JSONLs.
//!
//! Upstream counterpart: `thread/address-book.ts`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::adapter::{EndpointLiveness as GatewayLiveness, GatewayEndpointRef};
use super::addressing::{ThreadAddressEntry, normalize_thread_name};
use super::contracts_results::{ThreadEndpointSummary, ThreadStatus, ThreadSummary};
use super::endpoint_registry::EndpointKind;
use super::engine::GatewayAddressEntry;
use super::session_facts::{
    SessionFacts, read_session_facts, read_session_facts_async, thread_title as facts_thread_title,
};
use super::types::GatewayEndpointKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostListSessions {
    pub sessions: Option<Vec<HostSession>>,
    pub error: Option<String>,
}

impl HostListSessions {
    pub fn ok(sessions: Vec<HostSession>) -> Self {
        Self {
            sessions: Some(sessions),
            error: None,
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            sessions: None,
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressBookHost {
    pub socket: String,
    pub list_sessions: Option<HostListSessions>,
    pub error: Option<String>,
    pub endpoint_kind: GatewayEndpointKind,
    pub alive: Option<bool>,
    pub reason: Option<String>,
    pub legacy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    pub archived: bool,
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
    pub archived: bool,
}

pub fn thread_title(name: Option<&str>, first_user_text: Option<&str>) -> Option<String> {
    facts_thread_title(name, first_user_text)
}

pub fn derive_surface(
    endpoint_kind: Option<GatewayEndpointKind>,
    session_kind: Option<&str>,
    socket: &str,
) -> ThreadSurface {
    if endpoint_kind == Some(GatewayEndpointKind::Tui) {
        return ThreadSurface::Tui;
    }
    if session_kind == Some("worker") {
        return ThreadSurface::Child;
    }
    let name = Path::new(socket)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if name.starts_with("i-") {
        ThreadSurface::Desktop
    } else if name.starts_with("p-") {
        ThreadSurface::Child
    } else {
        ThreadSurface::Daemon
    }
}

pub fn to_gateway_endpoint_kind(kind: EndpointKind) -> GatewayEndpointKind {
    match kind {
        EndpointKind::RpcHost => GatewayEndpointKind::RpcHost,
        EndpointKind::Tui => GatewayEndpointKind::Tui,
    }
}

fn host_failure(host: &AddressBookHost) -> Option<String> {
    if host.reason.as_deref() == Some("live_unresponsive") {
        return Some("live_unresponsive".to_owned());
    }
    if let Some(error) = &host.error {
        return Some(error.clone());
    }
    match &host.list_sessions {
        None => Some("host unavailable".to_owned()),
        Some(outcome) => outcome.error.clone(),
    }
}

fn from_disk(
    session: &DiskSession,
    host: Option<&AddressBookHost>,
    failure: Option<&str>,
) -> AddressEntry {
    let endpoint = session.source_host.as_ref().map(|socket| AddressEndpoint {
        kind: host
            .map(|h| h.endpoint_kind)
            .unwrap_or(GatewayEndpointKind::RpcHost),
        socket: socket.clone(),
        routing_id: None,
        durable_session_id: session.durable_id.clone(),
    });
    let surface = endpoint
        .as_ref()
        .map(|ep| derive_surface(Some(ep.kind), None, &ep.socket));
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
        endpoint,
        surface,
        alive: false,
        title: thread_title(session.name.as_deref(), session.first_user_text.as_deref()),
        archived: session.archived,
    }
}

/// Assemble one fresh view from endpoints and disk sessions.
pub fn assemble_address_book(
    hosts: &[AddressBookHost],
    disk_sessions: &[DiskSession],
    assembled_at: &str,
) -> Vec<AddressEntry> {
    assemble_address_book_with_facts(hosts, disk_sessions, assembled_at, |p| {
        read_session_facts(Path::new(p))
    })
}

pub fn assemble_address_book_with_facts<F>(
    hosts: &[AddressBookHost],
    disk_sessions: &[DiskSession],
    assembled_at: &str,
    facts_fn: F,
) -> Vec<AddressEntry>
where
    F: Fn(&str) -> Option<SessionFacts>,
{
    let mut by_durable_id: HashMap<String, AddressEntry> = HashMap::new();
    let mut failures: HashMap<String, String> = HashMap::new();
    let by_socket: HashMap<&str, &AddressBookHost> =
        hosts.iter().map(|h| (h.socket.as_str(), h)).collect();

    for host in hosts {
        if let Some(failure) = host_failure(host) {
            failures.insert(host.socket.clone(), failure);
        }
    }

    for session in disk_sessions {
        if session.durable_id.is_empty() {
            continue;
        }
        if session.source_host.is_none()
            && by_durable_id
                .get(&session.durable_id)
                .and_then(|e| e.source_host.as_ref())
                .is_some()
        {
            continue;
        }
        let failure = session
            .source_host
            .as_ref()
            .and_then(|socket| failures.get(socket).map(String::as_str));
        let host = session
            .source_host
            .as_ref()
            .and_then(|socket| by_socket.get(socket.as_str()).copied());
        by_durable_id.insert(
            session.durable_id.clone(),
            from_disk(session, host, failure),
        );
    }

    for host in hosts {
        let Some(sessions) = host
            .list_sessions
            .as_ref()
            .and_then(|outcome| outcome.sessions.as_ref())
        else {
            continue;
        };
        for session in sessions {
            if session.status == Some(HostSessionStatus::Closed) {
                continue;
            }
            let durable_id = match &session.durable_session_id {
                Some(id) if !id.is_empty() => id.clone(),
                _ if host.endpoint_kind == GatewayEndpointKind::Tui => session.session_id.clone(),
                _ => continue,
            };
            if durable_id.is_empty() {
                continue;
            }
            let disk = by_durable_id.get(&durable_id).cloned();
            let session_path = session
                .session_path
                .clone()
                .or_else(|| disk.as_ref().and_then(|d| d.session_path.clone()));
            let needs_facts = session.name.is_none()
                || session.created_at.as_deref().is_none_or(str::is_empty)
                || session.updated_at.as_deref().is_none_or(str::is_empty);
            let facts = if needs_facts {
                session_path.as_deref().and_then(&facts_fn)
            } else {
                None
            };
            let created_at = session
                .created_at
                .as_deref()
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
                .or_else(|| facts.as_ref().map(|f| f.created_at.clone()))
                .or_else(|| disk.as_ref().map(|d| d.created_at.clone()))
                .unwrap_or_else(|| assembled_at.to_owned());
            let updated_at = session
                .updated_at
                .as_deref()
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    if let Some(f) = facts.as_ref() {
                        f.updated_at.clone()
                    } else {
                        disk.as_ref().and_then(|d| d.updated_at.clone())
                    }
                });
            let name = session
                .name
                .clone()
                .or_else(|| disk.as_ref().and_then(|d| d.name.clone()))
                .or_else(|| facts.as_ref().and_then(|f| f.name.clone()));
            let title = thread_title(
                name.as_deref(),
                facts.as_ref().and_then(|f| f.first_user_text.as_deref()),
            )
            .or_else(|| disk.as_ref().and_then(|d| d.title.clone()));
            let entry = AddressEntry {
                durable_id: durable_id.clone(),
                routing_id: Some(session.session_id.clone()),
                name,
                cwd: session.cwd.clone(),
                status: ThreadStatus::Live,
                liveness: ThreadStatus::Live,
                source_host: Some(host.socket.clone()),
                session_path,
                created_at,
                updated_at,
                thread_id: durable_id.clone(),
                error_note: None,
                endpoint: Some(AddressEndpoint {
                    kind: host.endpoint_kind,
                    socket: host.socket.clone(),
                    routing_id: Some(session.session_id.clone()),
                    durable_session_id: durable_id.clone(),
                }),
                surface: Some(derive_surface(
                    Some(host.endpoint_kind),
                    session.kind.as_deref(),
                    &host.socket,
                )),
                alive: host.alive != Some(false),
                title,
                archived: disk.as_ref().map(|d| d.archived).unwrap_or(false),
            };
            by_durable_id.insert(durable_id, entry);
        }
    }

    let mut entries: Vec<AddressEntry> = by_durable_id.into_values().collect();
    entries.sort_by(|left, right| match (&left.updated_at, &right.updated_at) {
        (None, None) => left.durable_id.cmp(&right.durable_id),
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(l), Some(r)) => {
            if l != r {
                r.cmp(l)
            } else {
                left.durable_id.cmp(&right.durable_id)
            }
        }
    });
    entries
}

fn session_ids_of(thread_id: &str, routing_id: Option<&str>) -> Option<Vec<String>> {
    let rid = routing_id.filter(|s| !s.is_empty())?;
    if thread_id.is_empty() || rid == thread_id {
        return None;
    }
    Some(vec![rid.to_owned()])
}

pub fn to_thread_address_entries(entries: &[AddressEntry]) -> Vec<ThreadAddressEntry> {
    entries
        .iter()
        .map(|entry| {
            let endpoint = entry.endpoint.as_ref().map(|ep| ThreadEndpointSummary {
                kind: ep.kind.as_str().to_owned(),
                socket: ep.socket.clone(),
                routing_id: ep.routing_id.clone(),
                durable_session_id: ep.durable_session_id.clone(),
            });
            let session_ids = session_ids_of(
                &entry.thread_id,
                entry
                    .endpoint
                    .as_ref()
                    .and_then(|e| e.routing_id.as_deref()),
            );
            ThreadAddressEntry {
                summary: ThreadSummary {
                    thread_id: entry.thread_id.clone(),
                    name: entry.name.clone().unwrap_or_default(),
                    status: entry.status,
                    cwd: entry.cwd.clone(),
                    created_at: entry.created_at.clone(),
                    updated_at: entry.updated_at.clone(),
                    error_note: entry.error_note.clone(),
                    endpoint,
                    surface: entry.surface.map(|s| s.as_str().to_owned()),
                    alive: Some(entry.alive),
                    controls: None,
                    skills: None,
                    device: None,
                    alias: None,
                },
                preview: None,
                device: None,
                aliases: None,
                session_ids,
                remote_row_key: None,
            }
        })
        .collect()
}

/// The gateway engine's view: a thread is `routable` only while an endpoint that answered lists it.
pub fn to_gateway_address_entries(entries: &[AddressEntry]) -> Vec<GatewayAddressEntry> {
    entries
        .iter()
        .map(|entry| GatewayAddressEntry {
            thread_id: entry.thread_id.clone(),
            name: entry.name.clone().unwrap_or_default(),
            endpoint: entry.endpoint.as_ref().map(|ep| GatewayEndpointRef {
                kind: ep.kind,
                socket: ep.socket.clone(),
                routing_id: ep.routing_id.clone(),
                durable_session_id: ep.durable_session_id.clone(),
            }),
            liveness: if entry.alive {
                GatewayLiveness::Routable
            } else if entry.error_note.as_deref() == Some("live_unresponsive") {
                GatewayLiveness::LiveUnresponsive
            } else {
                GatewayLiveness::Dead
            },
        })
        .collect()
}

fn disk_session_of(path: &Path, facts: SessionFacts, source_host: Option<&str>) -> DiskSession {
    DiskSession {
        durable_id: facts.durable_id,
        name: facts.name,
        cwd: facts.cwd,
        created_at: facts.created_at,
        updated_at: facts.updated_at,
        session_path: path.to_string_lossy().into_owned(),
        source_host: source_host.map(str::to_owned),
        first_user_text: facts.first_user_text,
        archived: facts.archived,
    }
}

/// Read one session file's durable identity via bounded `read_session_facts`.
pub fn read_disk_session(path: &Path, source_host: Option<&str>) -> Option<DiskSession> {
    let facts = read_session_facts(path)?;
    Some(disk_session_of(path, facts, source_host))
}

pub async fn read_disk_session_async(
    path: &Path,
    source_host: Option<&str>,
) -> Option<DiskSession> {
    let facts = read_session_facts_async(path).await?;
    Some(disk_session_of(path, facts, source_host))
}

/// Encoded session directory prefix for a cwd (`--home-indo-work-`).
pub fn session_directory_prefix(root: &str) -> String {
    let resolved = Path::new(root).to_string_lossy();
    let stripped = resolved.trim_start_matches(['/', '\\']);
    let encoded: String = stripped
        .chars()
        .map(|ch| {
            if matches!(ch, '/' | '\\' | ':') {
                '-'
            } else {
                ch
            }
        })
        .collect();
    format!("--{encoded}-")
}

fn project_directories(sessions_dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(sessions_dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|entry| {
            entry
                .file_type()
                .map(|ft| ft.is_dir() || ft.is_symlink())
                .unwrap_or(false)
        })
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| name.starts_with("--") && name.ends_with("--") && name.len() >= 4)
        .collect()
}

fn session_files(directory: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    rd.flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct FindDiskSessionsOptions {
    pub all_scope: bool,
    pub workspace_roots: Vec<String>,
    pub id_only: bool,
}

/// Look up session files by durable id or workspace-scoped normalized name.
pub fn find_disk_sessions(
    sessions_dir: &Path,
    address: &str,
    all_scope: bool,
    workspace_roots: &[String],
) -> Vec<DiskSession> {
    find_disk_sessions_with_options(
        sessions_dir,
        address,
        &FindDiskSessionsOptions {
            all_scope,
            workspace_roots: workspace_roots.to_vec(),
            id_only: false,
        },
    )
}

pub fn find_disk_sessions_with_options(
    sessions_dir: &Path,
    address: &str,
    opts: &FindDiskSessionsOptions,
) -> Vec<DiskSession> {
    let project_dirs = project_directories(sessions_dir);
    let mut cache: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let id_suffix = format!("_{address}.jsonl");
    let mut by_id = Vec::new();
    for dir in &project_dirs {
        let files = cache
            .entry(dir.clone())
            .or_insert_with(|| session_files(&sessions_dir.join(dir)));
        for file in files {
            let matches_name = file
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&id_suffix));
            if matches_name
                && let Some(session) = read_disk_session(file, None)
                && session.durable_id == address
            {
                by_id.push(session);
            }
        }
    }
    if !by_id.is_empty() || opts.id_only {
        return by_id;
    }

    let wanted = normalize_thread_name(address);
    if wanted.is_empty() {
        return Vec::new();
    }
    let prefixes: Option<Vec<String>> = (!opts.all_scope).then(|| {
        opts.workspace_roots
            .iter()
            .map(|r| session_directory_prefix(r))
            .collect()
    });
    let mut by_name = Vec::new();
    for dir in &project_dirs {
        if let Some(prefs) = &prefixes
            && !prefs.iter().any(|prefix| dir.starts_with(prefix))
        {
            continue;
        }
        let files = cache
            .entry(dir.clone())
            .or_insert_with(|| session_files(&sessions_dir.join(dir)));
        for file in files {
            if let Some(session) = read_disk_session(file, None)
                && let Some(name) = session.name.as_deref()
                && normalize_thread_name(name) == wanted
            {
                by_name.push(session);
            }
        }
    }
    by_name
}

pub fn scan_disk_sessions(sessions_dir: &Path, source_host: Option<&str>) -> Vec<DiskSession> {
    let mut found: Vec<DiskSession> = project_directories(sessions_dir)
        .into_iter()
        .flat_map(|dir| session_files(&sessions_dir.join(dir)))
        .filter_map(|file| read_disk_session(&file, source_host))
        .collect();
    found.sort_by(|left, right| match (&left.updated_at, &right.updated_at) {
        (None, None) => left.durable_id.cmp(&right.durable_id),
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(l), Some(r)) => {
            if l != r {
                r.cmp(l)
            } else {
                left.durable_id.cmp(&right.durable_id)
            }
        }
    });
    found
}

/// Reads unique claimed disk sessions for an endpoint directory when its listing failed.
pub fn read_claimed_disk_sessions(
    claimed_paths: &[String],
    source_host: Option<&str>,
) -> Vec<DiskSession> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for path in claimed_paths {
        if !seen.insert(path.clone()) {
            continue;
        }
        if let Some(session) = read_disk_session(Path::new(path), source_host) {
            out.push(session);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(durable_id: &str) -> DiskSession {
        DiskSession {
            durable_id: durable_id.to_owned(),
            name: Some("worker".to_owned()),
            cwd: "/repo".to_owned(),
            created_at: "2026-01-01T00:00:00.000Z".to_owned(),
            updated_at: Some("2026-01-02T00:00:00.000Z".to_owned()),
            session_path: "/tmp/session.jsonl".to_owned(),
            source_host: Some("/run/host.sock".to_owned()),
            first_user_text: None,
            archived: false,
        }
    }

    fn host(sessions: Vec<HostSession>) -> AddressBookHost {
        AddressBookHost {
            socket: "/run/host.sock".to_owned(),
            list_sessions: Some(HostListSessions::ok(sessions)),
            error: None,
            endpoint_kind: GatewayEndpointKind::RpcHost,
            alive: Some(true),
            reason: None,
            legacy: false,
        }
    }

    fn live_session() -> HostSession {
        HostSession {
            session_id: "rpc-1".to_owned(),
            durable_session_id: Some("d-1".to_owned()),
            session_path: Some("/tmp/session.jsonl".to_owned()),
            cwd: "/repo".to_owned(),
            name: Some("worker".to_owned()),
            status: Some(HostSessionStatus::Open),
            kind: Some("interactive".to_owned()),
            created_at: Some("2026-01-01T00:00:00.000Z".to_owned()),
            updated_at: Some("2026-01-03T00:00:00.000Z".to_owned()),
        }
    }

    #[test]
    fn a_live_host_listing_turns_a_disk_session_into_a_live_routable_entry() {
        let entries = assemble_address_book(
            &[host(vec![live_session()])],
            &[disk("d-1")],
            "2026-01-04T00:00:00.000Z",
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, ThreadStatus::Live);
        assert!(entries[0].alive);
        let gateway = to_gateway_address_entries(&entries);
        assert_eq!(gateway[0].liveness, GatewayLiveness::Routable);
        let addr = to_thread_address_entries(&entries);
        assert_eq!(addr[0].session_ids, Some(vec!["rpc-1".to_owned()]));
    }

    #[test]
    fn a_dead_host_falls_back_to_the_disk_session_with_its_error_note() {
        let mut dead = host(Vec::new());
        dead.list_sessions = Some(HostListSessions::failed("ECONNREFUSED"));
        dead.alive = Some(false);
        let entries = assemble_address_book(&[dead], &[disk("d-1")], "2026-01-04T00:00:00.000Z");
        assert_eq!(entries[0].status, ThreadStatus::Resumable);
        assert_eq!(entries[0].error_note.as_deref(), Some("ECONNREFUSED"));
        assert_eq!(
            to_gateway_address_entries(&entries)[0].liveness,
            GatewayLiveness::Dead
        );
    }

    #[test]
    fn a_live_unresponsive_endpoint_stays_unresponsive_never_gone() {
        let mut unresponsive = host(Vec::new());
        unresponsive.alive = Some(false);
        unresponsive.reason = Some("live_unresponsive".to_owned());
        let entries =
            assemble_address_book(&[unresponsive], &[disk("d-1")], "2026-01-04T00:00:00.000Z");
        assert_eq!(
            to_gateway_address_entries(&entries)[0].liveness,
            GatewayLiveness::LiveUnresponsive
        );
    }

    #[test]
    fn the_surface_follows_what_serves_the_session() {
        assert_eq!(
            derive_surface(Some(GatewayEndpointKind::Tui), None, "/run/tui-1.sock"),
            ThreadSurface::Tui
        );
        assert_eq!(
            derive_surface(
                Some(GatewayEndpointKind::RpcHost),
                Some("worker"),
                "/run/x.sock"
            ),
            ThreadSurface::Child
        );
        assert_eq!(
            derive_surface(Some(GatewayEndpointKind::RpcHost), None, "/run/i-7.sock"),
            ThreadSurface::Desktop
        );
        assert_eq!(
            derive_surface(Some(GatewayEndpointKind::RpcHost), None, "/run/p-7.sock"),
            ThreadSurface::Child
        );
        assert_eq!(
            derive_surface(Some(GatewayEndpointKind::RpcHost), None, "/run/host.sock"),
            ThreadSurface::Daemon
        );
    }

    #[test]
    fn a_title_never_falls_back_to_the_durable_id() {
        assert_eq!(
            thread_title(Some("Build"), Some("first words")),
            Some("Build".to_owned())
        );
        assert_eq!(
            thread_title(None, Some("first words")),
            Some("first words".to_owned())
        );
        assert_eq!(thread_title(None, None), None);
    }

    #[test]
    fn a_session_directory_prefix_encodes_the_workspace() {
        assert_eq!(
            session_directory_prefix("/home/indo/work"),
            "--home-indo-work-"
        );
    }

    #[test]
    fn find_disk_sessions_id_only_skips_name_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("--repo--");
        std::fs::create_dir_all(&proj).unwrap();
        let file = proj.join("2026-01-01_id-1.jsonl");
        std::fs::write(
            &file,
            r#"{"type":"session","id":"id-1","cwd":"/repo","timestamp":"2026-01-01T00:00:00Z"}
{"type":"session_info","name":"TargetName","timestamp":"2026-01-01T00:00:01Z"}
"#,
        )
        .unwrap();
        let by_id = find_disk_sessions_with_options(
            dir.path(),
            "id-1",
            &FindDiskSessionsOptions {
                id_only: true,
                ..Default::default()
            },
        );
        assert_eq!(by_id.len(), 1);
        let skipped = find_disk_sessions_with_options(
            dir.path(),
            "TargetName",
            &FindDiskSessionsOptions {
                all_scope: true,
                id_only: true,
                ..Default::default()
            },
        );
        assert!(skipped.is_empty());
    }

    #[test]
    fn merges_live_sessions_from_two_reachable_hosts() {
        let mut h1 = host(vec![live_session()]);
        h1.socket = "/run/h1.sock".into();
        let mut s2 = live_session();
        s2.session_id = "rpc-2".into();
        s2.durable_session_id = Some("d-2".into());
        s2.name = Some("second".into());
        let mut h2 = host(vec![s2]);
        h2.socket = "/run/h2.sock".into();
        let entries = assemble_address_book(&[h1, h2], &[], "2026-01-04T00:00:00.000Z");
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.status == ThreadStatus::Live));
    }

    #[test]
    fn sorts_newest_updated_at_first_and_durable_id_ascending_on_ties() {
        let mut s1 = live_session();
        s1.durable_session_id = Some("d-b".into());
        s1.updated_at = Some("2026-01-02T00:00:00.000Z".into());
        let mut s2 = live_session();
        s2.session_id = "rpc-2".into();
        s2.durable_session_id = Some("d-a".into());
        s2.updated_at = Some("2026-01-02T00:00:00.000Z".into());
        let mut s3 = live_session();
        s3.session_id = "rpc-3".into();
        s3.durable_session_id = Some("d-newest".into());
        s3.updated_at = Some("2026-01-05T00:00:00.000Z".into());
        let entries =
            assemble_address_book(&[host(vec![s1, s2, s3])], &[], "2026-01-06T00:00:00.000Z");
        let ids: Vec<&str> = entries.iter().map(|e| e.durable_id.as_str()).collect();
        assert_eq!(ids, vec!["d-newest", "d-a", "d-b"]);
    }

    #[test]
    fn includes_disk_only_sessions_and_drops_closed_live_sessions() {
        let mut closed = live_session();
        closed.status = Some(HostSessionStatus::Closed);
        let mut d_only = disk("d-disk");
        d_only.name = None;
        d_only.source_host = None;
        let entries =
            assemble_address_book(&[host(vec![closed])], &[d_only], "2026-01-04T00:00:00.000Z");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].durable_id, "d-disk");
        assert!(entries[0].name.is_none());
        assert_eq!(entries[0].status, ThreadStatus::Resumable);
    }

    #[test]
    fn terminal_endpoint_without_durable_id_uses_session_id_as_tui_thread() {
        let mut tui_s = live_session();
        tui_s.session_id = "tui-main".into();
        tui_s.durable_session_id = None;
        let mut h = host(vec![tui_s]);
        h.socket = "/run/t-0123456789abcdef.sock".into();
        h.endpoint_kind = GatewayEndpointKind::Tui;
        let entries = assemble_address_book(&[h], &[], "2026-01-04T00:00:00.000Z");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].thread_id, "tui-main");
        assert_eq!(entries[0].surface, Some(ThreadSurface::Tui));
    }

    #[test]
    fn host_with_none_list_sessions_marks_disk_entry_unavailable() {
        let mut down = host(Vec::new());
        down.list_sessions = None;
        down.alive = Some(false);
        let entries = assemble_address_book(&[down], &[disk("d-1")], "2026-01-04T00:00:00.000Z");
        assert_eq!(entries[0].error_note.as_deref(), Some("host unavailable"));
        assert!(!entries[0].alive);
    }

    #[test]
    fn disk_source_host_keeps_precedence_over_bare_disk_duplicate() {
        let mut bare = disk("d-1");
        bare.source_host = None;
        bare.name = Some("Old".into());
        let mut hosted = disk("d-1");
        hosted.source_host = Some("/run/host.sock".into());
        hosted.name = Some("Current".into());
        let entries = assemble_address_book(&[], &[hosted, bare], "2026-01-04T00:00:00.000Z");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name.as_deref(), Some("Current"));
    }

    #[test]
    fn session_facts_fill_missing_live_name_and_timestamps_from_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("2026-01-01_d-1.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"session","id":"d-1","cwd":"/repo","timestamp":"2026-01-01T00:00:00Z"}
{"type":"message","message":{"role":"user","content":"Analyze perf bug"},"timestamp":"2026-01-01T00:01:00Z"}
{"type":"session_info","name":"PerfSession","timestamp":"2026-01-01T00:02:00Z"}
"#,
        )
        .unwrap();
        let mut s = live_session();
        s.name = None;
        s.created_at = None;
        s.updated_at = None;
        s.session_path = Some(path.to_string_lossy().into_owned());
        let entries = assemble_address_book(&[host(vec![s])], &[], "2026-01-04T00:00:00.000Z");
        assert_eq!(entries[0].name.as_deref(), Some("PerfSession"));
        assert_eq!(
            entries[0].updated_at.as_deref(),
            Some("2026-01-01T00:02:00Z")
        );
    }

    #[test]
    fn archived_disk_flag_propagates_to_live_entry() {
        let mut d = disk("d-1");
        d.archived = true;
        let entries = assemble_address_book(
            &[host(vec![live_session()])],
            &[d],
            "2026-01-04T00:00:00.000Z",
        );
        assert!(entries[0].archived);
    }
}
