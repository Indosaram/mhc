//! The thread tools' client for every endpoint an agent directory holds.
//!
//! Upstream counterpart: `thread/live-surface.ts`. The wire client is the `RpcTransport` seam: this
//! module owns the surface logic (which endpoint is dialed, which session a command names, which
//! refusal a reply means), and `UnixJsonlTransport` speaks authenticated JSONL over Unix sockets.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::adapter::{
    BoxFuture, EndpointLiveness, GatewayEndpointPort, GatewayEndpointRef, ReleaseSessionReply,
    UiAnswer, UiAnswerReply,
};
use super::address_book::{
    AddressBookHost, HostListSessions, HostSession, HostSessionStatus, read_disk_session,
    to_gateway_endpoint_kind,
};
use super::attachment_fence::{
    FenceOutcome, RpcTransport, fenced_command, fenced_command_with_options,
};
use super::control::close_ladder::{RecordAnswer, close_ladder, is_session_gone};
use super::control::port::ControlError;
use super::endpoint_controls::{accepted_controls, legacy_tui_commands, tui_commands_from};
use super::endpoint_registry::{
    EndpointKind, claimed_session_paths, control_socket_secret_path, is_tui_control_socket,
    list_registry_endpoints,
};
use super::gateway_replies::{release_reply, wake_reply};
use super::initial_skills::INITIAL_SKILLS_CONTEXT_KEY;
use super::live_surface_paths::resolve_thread_socket;
use super::reader::ThreadTranscriptEntry;
use super::tools::model_match::ModelChoice;
use super::tools::ports::{
    InterruptRequest, OpenSessionParams, PromptDeliveryOptions, SessionStateSnapshot, SessionWatch,
    SessionWatchHandlers, SessionWatchInitial, SessionWatchOutcome, SessionWatchStatusChange,
    ThreadCloseAnswer, ThreadCloseOptions, ThreadHost, ThreadHostSession, ThreadHostView,
    ThreadHostViewRequest, ThreadSessionPort,
};
use super::types::GatewayEndpointKind;

pub const REQUEST_TIMEOUT_MS: u64 = 60_000;
/// Budget for one endpoint's `list_sessions` while enumerating: one hung shard must not stall every call.
pub const ENDPOINT_LIST_TIMEOUT_MS: u64 = 10_000;
/// A terminal answers from the user's own process: a suspended or busy one costs a listing at most this long.
pub const TUI_REQUEST_TIMEOUT_MS: u64 = 1_500;
/// Margin past graceful close drain timeout for answer arrival.
const LADDER_ANSWER_MARGIN_MS: u64 = 5_000;
/// How long one `host status --all` enumeration is reused before the engine is asked again.
pub const HOST_ENDPOINTS_CACHE_TTL_MS: i64 = 5_000;
pub const HOST_STATUS_ALL_ARGS: [&str; 5] =
    ["host", "status", "--all", "--include-workers", "--json"];

/// The commands any terminal control endpoint answers, an older one included.
pub const LEGACY_TUI_COMMANDS: [&str; 5] = [
    "list_sessions",
    "get_messages",
    "get_state",
    "get_protocol_info",
    "release_session",
];

#[derive(Debug, Clone, Default)]
pub struct RpcCallOptions {
    pub timeout_ms: Option<u64>,
    pub secret: Option<Vec<u8>>,
    pub key_root: Option<PathBuf>,
    pub durable_session_id: Option<String>,
    pub opened_name: Option<String>,
    pub rpc_host: bool,
    pub attached: bool,
}

/// Sends one correlated JSONL request on an authenticated Unix-domain socket (with optional capability fence).
pub async fn request_frame(
    socket_path: &str,
    command: Value,
    options: RpcCallOptions,
) -> Result<Value, String> {
    #[cfg(not(unix))]
    {
        let _ = (socket_path, command, options);
        Err("host_unavailable:unix sockets are not supported on this platform".to_owned())
    }
    #[cfg(unix)]
    {
        use tokio::io::AsyncWriteExt;

        let timeout_ms = options.timeout_ms.unwrap_or(REQUEST_TIMEOUT_MS);
        let cmd_type = command
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();

        if !options.rpc_host {
            let mut sock = maho_rpc::socket_client::RpcSocket::connect(
                socket_path,
                Duration::from_millis(timeout_ms),
            )
            .await
            .map_err(|e| format!("host_unavailable:{socket_path}: {e}"))?;
            if let Some(secret) = &options.secret {
                let mut stream = sock
                    .into_stream()
                    .map_err(|e| format!("host_unavailable:{socket_path}: {e}"))?;
                stream
                    .write_all(secret)
                    .await
                    .map_err(|e| format!("host_unavailable:{socket_path}: {e}"))?;
                // Re-wrap stream inside a fresh RpcSocket-equivalent read loop.
                return request_over_raw_stream(
                    stream,
                    command,
                    Duration::from_millis(timeout_ms),
                    socket_path,
                )
                .await;
            }
            return sock
                .request(command)
                .await
                .map_err(|e| format!("thread RPC request failed: {e}"));
        }

        let probe_cmd = if cmd_type == "get_protocol_info" {
            command.clone()
        } else {
            json!({ "type": "get_protocol_info", "observe": true })
        };
        let (sock, info) = if options.secret.is_none() {
            maho_rpc::host_probe::authenticated_host_socket(
                socket_path,
                &probe_cmd,
                timeout_ms,
                options.key_root.as_deref(),
            )
            .await
            .map_err(|e| format!("host_unavailable:{socket_path}: {e}"))?
        } else {
            let mut s = maho_rpc::socket_client::RpcSocket::connect(
                socket_path,
                Duration::from_millis(timeout_ms),
            )
            .await
            .map_err(|e| format!("host_unavailable:{socket_path}: {e}"))?;
            let info = s
                .request(probe_cmd)
                .await
                .map_err(|e| format!("thread RPC request failed: {e}"))?;
            (s, info)
        };

        if cmd_type == "get_protocol_info" {
            return Ok(info);
        }

        let protocol = info.get("data").and_then(Value::as_object);
        let capabilities: BTreeSet<String> = protocol
            .and_then(|d| d.get("capabilities"))
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        let conn_transport = Arc::new(ConnectionTransport {
            socket: Arc::new(tokio::sync::Mutex::new(sock)),
        });
        let send_cmd = command.clone();
        let opened_name = options.opened_name.clone();
        let ct_ref = conn_transport.clone();
        let result = fenced_command_with_options(
            &*ct_ref,
            socket_path,
            command,
            &capabilities,
            options.durable_session_id.as_deref(),
            options.attached,
            &|| {
                let cmd = send_cmd.clone();
                let opened_name = opened_name.clone();
                let conn_transport = conn_transport.clone();
                Box::pin(async move {
                    let frame = conn_transport.send_raw(cmd.clone()).await?;
                    if cmd.get("type").and_then(Value::as_str) == Some("open_session")
                        && frame.get("success").and_then(Value::as_bool) == Some(true)
                        && let Some(name) = opened_name.filter(|n| !n.trim().is_empty())
                    {
                        let opened = data_record(&frame, "open_session")?;
                        if let Some(sid) = opened.get("sessionId").and_then(Value::as_str) {
                            let rename_frame = conn_transport
                                .send_raw(json!({
                                    "type": "set_session_name",
                                    "sessionId": sid,
                                    "name": name,
                                }))
                                .await?;
                            data_record(&rename_frame, "set_session_name")?;
                        }
                    }
                    Ok(frame)
                })
            },
        )
        .await?;

        match result {
            FenceOutcome::Ran(frame) | FenceOutcome::Refused(frame) => Ok(frame),
            FenceOutcome::Failed(err) => Err(err),
        }
    }
}

#[cfg(unix)]
struct ConnectionTransport {
    socket: Arc<tokio::sync::Mutex<maho_rpc::socket_client::RpcSocket>>,
}

#[cfg(unix)]
impl ConnectionTransport {
    async fn send_raw(&self, command: Value) -> Result<Value, String> {
        let mut guard = self.socket.lock().await;
        guard
            .request(command)
            .await
            .map_err(|e| format!("thread RPC request failed: {e}"))
    }
}

#[cfg(unix)]
impl RpcTransport for ConnectionTransport {
    fn request(
        &self,
        _socket: &str,
        command: Value,
        _durable_session_id: Option<String>,
    ) -> BoxFuture<Result<Value, String>> {
        let socket = self.socket.clone();
        Box::pin(async move {
            let mut guard = socket.lock().await;
            guard
                .request(command)
                .await
                .map_err(|e| format!("thread RPC request failed: {e}"))
        })
    }
}

#[cfg(unix)]
async fn request_over_raw_stream(
    stream: tokio::net::UnixStream,
    mut command: Value,
    timeout: Duration,
    socket_path: &str,
) -> Result<Value, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let deadline = tokio::time::Instant::now() + timeout;
    let id = command
        .get("id")
        .cloned()
        .unwrap_or_else(|| Value::String(uuid::Uuid::new_v4().to_string()));
    if let Some(obj) = command.as_object_mut() {
        obj.insert("id".into(), id.clone());
    }
    let mut reader = BufReader::new(stream);
    tokio::time::timeout_at(deadline, async {
        let mut text = serde_json::to_vec(&command)
            .map_err(|e| format!("thread RPC serialize failed: {e}"))?;
        text.push(b'\n');
        reader
            .get_mut()
            .write_all(&text)
            .await
            .map_err(|e| format!("host_unavailable:{socket_path}: {e}"))?;
        loop {
            let mut bytes = Vec::new();
            let n = reader
                .read_until(b'\n', &mut bytes)
                .await
                .map_err(|e| format!("host_unavailable:{socket_path}: {e}"))?;
            if n == 0 {
                return Err(format!(
                    "host_unavailable:{socket_path}: host closed before response"
                ));
            }
            if let Ok(frame) = serde_json::from_slice::<Value>(&bytes)
                && frame.get("id") == Some(&id)
            {
                return Ok(frame);
            }
        }
    })
    .await
    .map_err(|_| "thread RPC request timed out".to_owned())?
}

/// Real local Unix-domain JSONL `RpcTransport` backed by `maho_rpc::host_probe::authenticated_host_socket`.
pub struct UnixJsonlTransport {
    pub agent_dir: PathBuf,
    pub timeout_ms: u64,
}

impl RpcTransport for UnixJsonlTransport {
    fn request(
        &self,
        socket: &str,
        command: Value,
        durable_session_id: Option<String>,
    ) -> BoxFuture<Result<Value, String>> {
        let socket = socket.to_owned();
        let key_root = self.agent_dir.clone();
        let timeout_ms = self.timeout_ms;
        Box::pin(async move {
            let tui = is_tui_control_socket(&socket);
            let secret = if tui {
                Some(
                    std::fs::read(control_socket_secret_path(&socket))
                        .map_err(|_| format!("host_unavailable:{socket}"))?,
                )
            } else {
                None
            };
            request_frame(
                &socket,
                command,
                RpcCallOptions {
                    timeout_ms: Some(if tui {
                        TUI_REQUEST_TIMEOUT_MS
                    } else {
                        timeout_ms
                    }),
                    secret,
                    key_root: Some(key_root),
                    durable_session_id,
                    opened_name: None,
                    rpc_host: !tui,
                    attached: false,
                },
            )
            .await
        })
    }
}

/// One endpoint row of `host status --all --include-workers --json`, reduced to what addressing reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEndpointReport {
    pub socket: Option<String>,
    pub reachable: bool,
    pub session_paths: Vec<String>,
    pub endpoint_kind: Option<GatewayEndpointKind>,
    pub alive: Option<bool>,
    pub reason: Option<String>,
}

fn strings_at(rows: Option<&Value>, field: &str) -> Vec<String> {
    rows.and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get(field)?.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn owner_session_path(owner: Option<&Value>) -> Vec<String> {
    owner
        .and_then(|owner| owner.get("session"))
        .and_then(|session| session.get("path"))
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(|path| vec![path.to_owned()])
        .unwrap_or_default()
}

/// Reads the one JSON line `host status --all` prints; anything without an `endpoints` array cannot enumerate.
pub fn parse_host_status_all(stdout: &str) -> Option<Vec<HostEndpointReport>> {
    let line = stdout
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())?;
    let parsed: Value = serde_json::from_str(line).ok()?;
    let endpoints = parsed.get("endpoints")?.as_array()?;
    Some(
        endpoints
            .iter()
            .filter_map(|endpoint| {
                endpoint.as_object()?;
                let mut session_paths = strings_at(endpoint.get("session_rows"), "session_path");
                session_paths.extend(strings_at(endpoint.get("claims"), "session_path"));
                session_paths.extend(owner_session_path(endpoint.get("owner")));
                session_paths.sort();
                session_paths.dedup();
                Some(HostEndpointReport {
                    socket: endpoint
                        .get("socket")
                        .and_then(Value::as_str)
                        .filter(|socket| !socket.is_empty())
                        .map(str::to_owned),
                    reachable: endpoint
                        .get("reachable")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    session_paths,
                    endpoint_kind: match endpoint.get("endpoint_kind").and_then(Value::as_str) {
                        Some("tui") => Some(GatewayEndpointKind::Tui),
                        Some("rpc_host") => Some(GatewayEndpointKind::RpcHost),
                        _ => None,
                    },
                    alive: endpoint.get("alive").and_then(Value::as_bool),
                    reason: endpoint
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            })
            .collect(),
    )
}

/// The frame a command's `data`, or the failure its reply names.
pub fn data_record(frame: &Value, command: &str) -> Result<Map<String, Value>, String> {
    let success = frame.get("success").and_then(Value::as_bool);
    let error = frame.get("error").and_then(Value::as_str);
    let error_code = frame.get("errorCode").and_then(Value::as_str);
    if success == Some(false)
        && command == "set_thinking_level"
        && error.is_some_and(|error| {
            error.starts_with("Thinking level ")
                && error.ends_with(" is not supported by the active model.")
        })
    {
        return Err(format!(
            "thinking_level_unsupported:{}",
            error.unwrap_or_default()
        ));
    }
    if success == Some(false) && error == Some("unsupported") {
        return Err(format!("unsupported:{command}"));
    }
    if success == Some(false)
        && command == "set_session_archived"
        && (error_code == Some("unknown_command")
            || error == Some("unknown_command")
            || error.is_some_and(|e| e.starts_with("Unknown command:")))
    {
        return Err("unsupported:set_session_archived".to_owned());
    }
    if success == Some(false)
        && (error_code == Some("auth_required") || error == Some("auth_required"))
    {
        return Err("auth_required".to_owned());
    }
    if success == Some(true)
        && frame.get("data").is_none()
        && matches!(
            command,
            "set_session_name"
                | "set_thinking_level"
                | "set_session_archived"
                | "close_session"
                | "prompt"
                | "interrupt"
        )
    {
        return Ok(Map::new());
    }
    match frame.get("data").and_then(Value::as_object) {
        Some(data) if success == Some(true) => Ok(data.clone()),
        _ => Err(format!(
            "thread RPC request failed: {}",
            frame
                .get("error")
                .map_or_else(|| frame.to_string(), Value::to_string)
        )),
    }
}

/// What the surface knows about one endpoint, as the last enumeration said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnownEndpoint {
    pub socket: String,
    pub paths: Vec<String>,
    pub kind: Option<GatewayEndpointKind>,
    pub alive: Option<bool>,
    pub reason: Option<String>,
}

pub struct LiveThreadSurfaceOptions {
    pub socket: String,
    pub transport: Arc<dyn RpcTransport>,
    /// Replaces the engine CLI enumeration; a test answers it without spawning anything.
    pub status_all: Option<Arc<dyn Fn() -> Option<Vec<HostEndpointReport>> + Send + Sync>>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
}

/// The thread tools' client: it never starts or replaces a host.
pub struct LiveThreadSurface {
    legacy: String,
    agent_dir: Option<PathBuf>,
    transport: Arc<dyn RpcTransport>,
    status_all: Arc<dyn Fn() -> Option<Vec<HostEndpointReport>> + Send + Sync>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    enumeration: Mutex<Option<(i64, Vec<KnownEndpoint>)>>,
    kinds: Mutex<BTreeMap<String, GatewayEndpointKind>>,
    last_listed: Mutex<BTreeMap<String, Vec<String>>>,
}

impl LiveThreadSurface {
    pub fn new(options: LiveThreadSurfaceOptions) -> Self {
        Self {
            legacy: options.socket,
            agent_dir: None,
            transport: options.transport,
            status_all: options.status_all.unwrap_or_else(|| Arc::new(|| None)),
            now: options.now.unwrap_or_else(|| {
                Arc::new(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|elapsed| elapsed.as_millis() as i64)
                        .unwrap_or(0)
                })
            }),
            enumeration: Mutex::new(None),
            kinds: Mutex::new(BTreeMap::new()),
            last_listed: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn new_real(agent_dir: PathBuf, legacy_socket: Option<String>) -> Self {
        let socket = legacy_socket.unwrap_or_else(|| resolve_thread_socket(&agent_dir));
        let transport: Arc<dyn RpcTransport> = Arc::new(UnixJsonlTransport {
            agent_dir: agent_dir.clone(),
            timeout_ms: REQUEST_TIMEOUT_MS,
        });
        Self {
            legacy: socket,
            agent_dir: Some(agent_dir),
            transport,
            status_all: Arc::new(|| None),
            now: Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_millis() as i64)
                    .unwrap_or(0)
            }),
            enumeration: Mutex::new(None),
            kinds: Mutex::new(BTreeMap::new()),
            last_listed: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn socket(&self) -> &str {
        &self.legacy
    }

    fn kind_of(&self, socket: &str) -> GatewayEndpointKind {
        self.kinds
            .lock()
            .ok()
            .and_then(|kinds| kinds.get(socket).copied())
            .unwrap_or_else(|| {
                if is_tui_control_socket(socket) {
                    GatewayEndpointKind::Tui
                } else {
                    GatewayEndpointKind::RpcHost
                }
            })
    }

    fn set_kind(&self, socket: &str, kind: GatewayEndpointKind) {
        if let Ok(mut kinds) = self.kinds.lock() {
            kinds.insert(socket.to_owned(), kind);
        }
    }

    pub fn known_endpoints(&self) -> Vec<KnownEndpoint> {
        if let Ok(cached) = self.enumeration.lock()
            && let Some((expires_at, endpoints)) = cached.as_ref()
            && *expires_at > (*self.now)()
        {
            return endpoints.clone();
        }
        let mut found: Vec<KnownEndpoint> = (self.status_all)()
            .map(|reports| {
                reports
                    .into_iter()
                    .filter_map(|report| {
                        Some(KnownEndpoint {
                            socket: report.socket?,
                            paths: report.session_paths,
                            kind: report.endpoint_kind,
                            alive: report.alive,
                            reason: report.reason,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        if found.is_empty()
            && let Some(dir) = &self.agent_dir
        {
            for reg in list_registry_endpoints(dir) {
                if let Some(sock) = reg.socket {
                    let paths = claimed_session_paths(&reg.dir);
                    found.push(KnownEndpoint {
                        socket: sock,
                        paths,
                        kind: Some(to_gateway_endpoint_kind(reg.endpoint_kind)),
                        alive: None,
                        reason: None,
                    });
                }
            }
        }

        if !found.iter().any(|endpoint| endpoint.socket == self.legacy) {
            found.insert(
                0,
                KnownEndpoint {
                    socket: self.legacy.clone(),
                    paths: Vec::new(),
                    kind: None,
                    alive: None,
                    reason: None,
                },
            );
        }
        if let Ok(mut cached) = self.enumeration.lock() {
            *cached = Some(((*self.now)() + HOST_ENDPOINTS_CACHE_TTL_MS, found.clone()));
        }
        found
    }

    pub fn call(
        &self,
        socket: &str,
        command: Value,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Value, String>> {
        self.transport
            .request(socket, command, durable_session_id.map(str::to_owned))
    }

    fn fenced(
        &self,
        socket: &str,
        command: Value,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Value, String>> {
        self.fenced_with_attached(socket, command, durable_session_id, false)
    }

    fn fenced_with_attached(
        &self,
        socket: &str,
        command: Value,
        durable_session_id: Option<&str>,
        attached: bool,
    ) -> BoxFuture<Result<Value, String>> {
        let socket = socket.to_owned();
        let kind = self.kind_of(&socket);
        let transport = self.transport.clone();
        let durable = durable_session_id.map(str::to_owned);
        let capabilities: BTreeSet<String> = [
            "attach_session",
            "release_attachment",
            "release_caller_aware",
        ]
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
        Box::pin(async move {
            if kind == GatewayEndpointKind::Tui {
                let command_type = command
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if !LEGACY_TUI_COMMANDS.contains(&command_type.as_str()) {
                    // Attempt request; real TUI may advertise more via get_protocol_info.
                    return transport.request(&socket, command, None).await;
                }
                return transport.request(&socket, command, None).await;
            }
            let send_command = command.clone();
            let send_transport = transport.clone();
            let send_socket = socket.clone();
            let send_durable = durable.clone();
            let send = move || {
                send_transport.request(&send_socket, send_command.clone(), send_durable.clone())
            };
            match fenced_command_with_options(
                transport.as_ref(),
                &socket,
                command,
                &capabilities,
                durable.as_deref(),
                attached,
                &send,
            )
            .await?
            {
                FenceOutcome::Ran(frame) | FenceOutcome::Refused(frame) => Ok(frame),
                FenceOutcome::Failed(error) => Err(error),
            }
        })
    }

    fn call_on(
        &self,
        socket: &str,
        command: &str,
        data: Value,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Value, String>> {
        let mut frame = data;
        if let Some(object) = frame.as_object_mut() {
            object.insert("type".into(), Value::String(command.to_owned()));
        }
        let command_name = command.to_owned();
        let pending = self.fenced(socket, frame, durable_session_id);
        Box::pin(async move {
            let frame = pending.await?;
            let data = data_record(&frame, &command_name)?;
            Ok(Value::Object(data))
        })
    }

    /// The routing id of a host session, looked up in that host's own listing when the endpoint has none.
    pub fn routing_id_of(
        &self,
        endpoint: &GatewayEndpointRef,
    ) -> BoxFuture<Result<Option<String>, String>> {
        if let Some(routing_id) = &endpoint.routing_id {
            let routing_id = routing_id.clone();
            return Box::pin(async move { Ok(Some(routing_id)) });
        }
        let socket = endpoint.socket.clone();
        let durable_id = endpoint.durable_session_id.clone();
        let listing = self.call_on(&socket, "list_sessions", json!({ "observe": true }), None);
        Box::pin(async move {
            let data = listing.await?;
            let sessions = data
                .get("sessions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(sessions.into_iter().find_map(|session| {
                let durable = session.get("durableSessionId").and_then(Value::as_str);
                let status = session.get("status").and_then(Value::as_str);
                (durable == Some(durable_id.as_str()) && status != Some("closed"))
                    .then(|| {
                        session
                            .get("sessionId")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .flatten()
            }))
        })
    }

    /// The gateway's sender port. A delivery is announced with `wake` only, never `prompt`.
    pub fn gateway(&self) -> GatewayEndpointPort {
        let surface = Arc::new(SurfaceHandle {
            transport: self.transport.clone(),
        });
        let wake_surface = surface.clone();
        let release_surface = surface.clone();
        let liveness_surface = surface.clone();
        let respond_surface = surface;
        GatewayEndpointPort {
            wake: Arc::new(move |endpoint, delivery_ids| {
                let surface = wake_surface.clone();
                Box::pin(async move {
                    let session_id = if endpoint.kind == GatewayEndpointKind::RpcHost {
                        surface.routing_id_of(&endpoint).await.ok().flatten()
                    } else {
                        None
                    };
                    if endpoint.kind == GatewayEndpointKind::RpcHost && session_id.is_none() {
                        return super::adapter::GatewayWakeReply::unroutable_no_live_session();
                    }
                    let mut command = json!({ "type": "wake", "delivery_ids": delivery_ids });
                    if let Some(session_id) = session_id
                        && let Some(object) = command.as_object_mut()
                    {
                        object.insert("sessionId".into(), Value::String(session_id));
                    }
                    match surface
                        .fenced(
                            &endpoint.socket,
                            command,
                            Some(&endpoint.durable_session_id),
                        )
                        .await
                    {
                        Ok(frame) => match data_record(&frame, "wake") {
                            Ok(data) => wake_reply(&Value::Object(data)),
                            Err(_) => {
                                super::adapter::GatewayWakeReply::unroutable_no_live_session()
                            }
                        },
                        Err(_) => super::adapter::GatewayWakeReply::unroutable_no_live_session(),
                    }
                })
            }),
            release_session: Some(Arc::new(move |endpoint, request| {
                let surface = release_surface.clone();
                Box::pin(async move {
                    if endpoint.kind != GatewayEndpointKind::RpcHost {
                        return ReleaseSessionReply::Failure {
                            error: "unsupported".to_owned(),
                            error_data: None,
                        };
                    }
                    let session_id = match surface.routing_id_of(&endpoint).await {
                        Ok(Some(session_id)) => session_id,
                        _ => {
                            return ReleaseSessionReply::Failure {
                                error: "unknown_session".to_owned(),
                                error_data: None,
                            };
                        }
                    };
                    let command = json!({
                        "type": "release_session",
                        "sessionId": session_id,
                        "reason": request.reason,
                        "interrupt": request.interrupt,
                        "force": request.force
                    });
                    match surface
                        .fenced(
                            &endpoint.socket,
                            command,
                            Some(&endpoint.durable_session_id),
                        )
                        .await
                    {
                        Ok(frame) => release_reply(&frame),
                        Err(error) => ReleaseSessionReply::Failure {
                            error,
                            error_data: None,
                        },
                    }
                })
            })),
            classify_liveness: Some(Arc::new(move |endpoint| {
                let surface = liveness_surface.clone();
                Box::pin(async move {
                    match surface
                        .call_on(
                            &endpoint.socket,
                            "get_protocol_info",
                            json!({ "observe": true }),
                            None,
                        )
                        .await
                    {
                        Ok(_) => EndpointLiveness::Routable,
                        Err(error) if error.starts_with("host_unavailable:") => {
                            EndpointLiveness::Dead
                        }
                        Err(_) => EndpointLiveness::LiveUnresponsive,
                    }
                })
            })),
            respond_ui: Some(Arc::new(move |endpoint, answer: UiAnswer| {
                let surface = respond_surface.clone();
                Box::pin(async move {
                    let session_id = if endpoint.kind == GatewayEndpointKind::RpcHost {
                        surface.routing_id_of(&endpoint).await.ok().flatten()
                    } else {
                        endpoint.routing_id.clone()
                    };
                    if endpoint.kind == GatewayEndpointKind::RpcHost && session_id.is_none() {
                        return UiAnswerReply::Refused {
                            error: "unknown_session".to_owned(),
                        };
                    }
                    let command = json!({
                        "type": "extension_ui_response",
                        "sessionId": session_id,
                        "uiRequestId": answer.ui_request_id,
                        "fields": answer.fields,
                    });
                    match surface
                        .fenced(
                            &endpoint.socket,
                            command,
                            Some(&endpoint.durable_session_id),
                        )
                        .await
                    {
                        Ok(frame)
                            if frame.get("success").and_then(Value::as_bool) == Some(true) =>
                        {
                            UiAnswerReply::Delivered
                        }
                        Ok(frame) => UiAnswerReply::Refused {
                            error: frame
                                .get("error")
                                .and_then(Value::as_str)
                                .unwrap_or("invalid_response")
                                .to_owned(),
                        },
                        Err(error) => UiAnswerReply::Refused { error },
                    }
                })
            })),
        }
    }

    /// The owner of one durable id, listed exactly as a listing lists it.
    pub fn list_target(
        &self,
        durable_id: &str,
        endpoint: &GatewayEndpointRef,
    ) -> BoxFuture<Result<Vec<Value>, String>> {
        self.set_kind(&endpoint.socket, endpoint.kind);
        let socket = endpoint.socket.clone();
        let durable_id = durable_id.to_owned();
        let listing = self.call_on(&socket, "list_sessions", json!({ "observe": true }), None);
        Box::pin(async move {
            let data = listing.await?;
            let sessions = data
                .get("sessions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(sessions
                .into_iter()
                .filter(|session| {
                    let durable = session
                        .get("durableSessionId")
                        .and_then(Value::as_str)
                        .or_else(|| session.get("sessionId").and_then(Value::as_str));
                    durable == Some(durable_id.as_str())
                        && session.get("status").and_then(Value::as_str) != Some("closed")
                })
                .collect())
        })
    }

    /// Opens a thread on the legacy endpoint and reads the session list back, so the reply carries the
    /// durable id and the name the address book keys entries by.
    pub fn open_session(
        &self,
        params: Value,
        name: Option<&str>,
    ) -> BoxFuture<Result<Value, String>> {
        let socket = self.legacy.clone();
        let name = name.map(str::to_owned);
        let transport = self.transport.clone();
        Box::pin(async move {
            let mut command = params;
            if let Some(object) = command.as_object_mut() {
                object.insert("type".into(), Value::String("open_session".into()));
                object.insert("retain_on_disconnect".into(), Value::Bool(true));
            }
            let requested_durable_id = command
                .get("durableSessionId")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let frame = transport.request(&socket, command, None).await?;
            let mut opened = data_record(&frame, "open_session")?;
            let routing_id = opened
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let listing = transport
                .request(
                    &socket,
                    json!({ "type": "list_sessions", "observe": true }),
                    None,
                )
                .await?;
            let listed = data_record(&listing, "list_sessions")?;
            let sessions = listed
                .get("sessions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if let Some(session) = sessions.into_iter().find(|session| {
                session.get("sessionId").and_then(Value::as_str) == Some(routing_id.as_str())
            }) && let Some(object) = session.as_object()
            {
                for (key, value) in object {
                    opened.insert(key.clone(), value.clone());
                }
            }
            let durable_id = opened
                .get("durableSessionId")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(requested_durable_id);
            if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
                let renamed = transport
                    .request(
                        &socket,
                        json!({ "type": "set_session_name", "sessionId": routing_id, "name": name }),
                        durable_id,
                    )
                    .await;
                if renamed.is_ok_and(|f| f.get("success").and_then(Value::as_bool) == Some(true)) {
                    opened.insert("name".into(), Value::String(name));
                }
            }
            opened.insert("sessionId".into(), Value::String(routing_id));
            opened.insert("socket".into(), Value::String(socket));
            opened.insert("endpoint_kind".into(), Value::String("rpc_host".into()));
            Ok(Value::Object(opened))
        })
    }

    fn parse_host_session(
        raw: &Value,
        socket: &str,
        kind: EndpointKind,
        controls: &[super::endpoint_controls::ThreadControl],
        version: Option<&str>,
    ) -> Option<ThreadHostSession> {
        let obj = raw.as_object()?;
        let session_id = obj.get("sessionId")?.as_str()?.to_owned();
        let durable = obj
            .get("durableSessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| (kind == EndpointKind::Tui).then(|| session_id.clone()));
        let model = obj.get("model").and_then(|m| {
            let mo = m.as_object()?;
            Some(ModelChoice {
                provider: mo.get("provider")?.as_str()?.to_owned(),
                id: mo.get("id")?.as_str()?.to_owned(),
                name: mo.get("name").and_then(Value::as_str).map(str::to_owned),
            })
        });
        let activity = obj
            .get("activity")
            .and_then(Value::as_str)
            .and_then(|a| match a {
                "idle" => Some(super::contracts::state::ThreadActivity::Idle),
                "running" => Some(super::contracts::state::ThreadActivity::Running),
                "awaiting_approval" => {
                    Some(super::contracts::state::ThreadActivity::AwaitingApproval)
                }
                "queued" => Some(super::contracts::state::ThreadActivity::Queued),
                "errored" => Some(super::contracts::state::ThreadActivity::Errored),
                _ => None,
            });
        Some(ThreadHostSession {
            session_id,
            durable_session_id: durable,
            session_path: obj
                .get("sessionPath")
                .and_then(Value::as_str)
                .map(str::to_owned),
            cwd: obj
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            name: obj.get("name").and_then(Value::as_str).map(str::to_owned),
            status: obj.get("status").and_then(Value::as_str).map(str::to_owned),
            kind: obj.get("kind").and_then(Value::as_str).map(str::to_owned),
            archived: obj.get("archived").and_then(Value::as_bool),
            activity,
            error_code: obj
                .get("error_code")
                .and_then(Value::as_str)
                .map(str::to_owned),
            turn_id: obj
                .get("turn_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            waiting: obj.get("waiting").cloned(),
            model,
            created_at: obj
                .get("createdAt")
                .or_else(|| obj.get("created_at"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            updated_at: obj
                .get("updatedAt")
                .or_else(|| obj.get("updated_at"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            endpoint_kind: Some(kind),
            controls: Some(controls.to_vec()),
            engine_version: version.map(str::to_owned),
            socket: Some(socket.to_owned()),
        })
    }

    fn endpoint_port(&self, socket: String) -> Arc<dyn ThreadSessionPort> {
        Arc::new(EndpointPort {
            socket,
            transport: self.transport.clone(),
            agent_dir: self.agent_dir.clone(),
        })
    }
}

struct EndpointPort {
    socket: String,
    transport: Arc<dyn RpcTransport>,
    agent_dir: Option<PathBuf>,
}

impl ThreadSessionPort for EndpointPort {
    fn get_messages(
        &self,
        session_id: &str,
    ) -> BoxFuture<Result<Vec<ThreadTranscriptEntry>, String>> {
        let socket = self.socket.clone();
        let transport = self.transport.clone();
        let sid = session_id.to_owned();
        Box::pin(async move {
            let frame = transport
                .request(
                    &socket,
                    json!({ "type": "get_messages", "sessionId": sid, "observe": true }),
                    None,
                )
                .await?;
            let data = data_record(&frame, "get_messages")?;
            let msgs = data
                .get("messages")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(msgs
                .into_iter()
                .filter_map(|v| v.as_object().cloned())
                .collect())
        })
    }

    fn get_state(
        &self,
        session_id: &str,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<SessionStateSnapshot, String>> {
        let socket = self.socket.clone();
        let transport = self.transport.clone();
        let sid = session_id.to_owned();
        let did = durable_session_id.map(str::to_owned);
        Box::pin(async move {
            let frame = transport
                .request(
                    &socket,
                    json!({ "type": "get_state", "sessionId": sid, "observe": true }),
                    did,
                )
                .await?;
            let data = data_record(&frame, "get_state")?;
            Ok(SessionStateSnapshot {
                is_streaming: data.get("isStreaming").and_then(Value::as_bool),
                active_turn_id: data
                    .get("activeTurnId")
                    .or_else(|| data.get("currentTurnId"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                model: data.get("model").cloned(),
                thinking_level: data.get("thinkingLevel").cloned(),
            })
        })
    }

    fn prompt(
        &self,
        session_id: &str,
        message: &str,
        options: Option<PromptDeliveryOptions>,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Option<String>, String>> {
        let socket = self.socket.clone();
        let transport = self.transport.clone();
        let sid = session_id.to_owned();
        let msg = message.to_owned();
        let did = durable_session_id.map(str::to_owned);
        Box::pin(async move {
            let mut cmd = json!({ "type": "prompt", "sessionId": sid, "message": msg });
            if let Some(behavior) = options.and_then(|o| o.streaming_behavior)
                && let Some(obj) = cmd.as_object_mut()
            {
                obj.insert("streamingBehavior".into(), Value::String(behavior));
            }
            let frame = transport.request(&socket, cmd, did).await?;
            let data = data_record(&frame, "prompt")?;
            Ok(data
                .get("turnId")
                .and_then(Value::as_str)
                .map(str::to_owned))
        })
    }

    fn interrupt(
        &self,
        session_id: &str,
        request: InterruptRequest,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Value, String>> {
        let socket = self.socket.clone();
        let transport = self.transport.clone();
        let sid = session_id.to_owned();
        let did = durable_session_id.map(str::to_owned);
        Box::pin(async move {
            let mut cmd = json!({
                "type": "interrupt",
                "sessionId": sid,
                "background": request.background,
            });
            if let Some(tid) = request.turn_id
                && let Some(obj) = cmd.as_object_mut()
            {
                obj.insert("turnId".into(), Value::String(tid));
            }
            let frame = transport.request(&socket, cmd, did).await?;
            let data = data_record(&frame, "interrupt")?;
            Ok(Value::Object(data))
        })
    }

    fn set_session_name(
        &self,
        session_id: &str,
        name: &str,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<(), String>> {
        let socket = self.socket.clone();
        let transport = self.transport.clone();
        let sid = session_id.to_owned();
        let name = name.to_owned();
        let did = durable_session_id.map(str::to_owned);
        Box::pin(async move {
            let frame = transport
                .request(
                    &socket,
                    json!({ "type": "set_session_name", "sessionId": sid, "name": name }),
                    did,
                )
                .await?;
            data_record(&frame, "set_session_name").map(|_| ())
        })
    }

    fn set_session_archived(
        &self,
        session_id: &str,
        archived: bool,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<(), String>> {
        let socket = self.socket.clone();
        let transport = self.transport.clone();
        let sid = session_id.to_owned();
        let did = durable_session_id.map(str::to_owned);
        Box::pin(async move {
            let frame = transport
                .request(
                    &socket,
                    json!({ "type": "set_session_archived", "sessionId": sid, "archived": archived }),
                    did,
                )
                .await?;
            data_record(&frame, "set_session_archived").map(|_| ())
        })
    }

    fn set_model(
        &self,
        session_id: &str,
        provider: &str,
        model_id: &str,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<ModelChoice, String>> {
        let socket = self.socket.clone();
        let transport = self.transport.clone();
        let sid = session_id.to_owned();
        let provider = provider.to_owned();
        let model_id = model_id.to_owned();
        let did = durable_session_id.map(str::to_owned);
        Box::pin(async move {
            let frame = transport
                .request(
                    &socket,
                    json!({
                        "type": "set_model",
                        "sessionId": sid,
                        "provider": provider,
                        "modelId": model_id,
                        "scope": "session",
                    }),
                    did,
                )
                .await?;
            let data = data_record(&frame, "set_model")?;
            Ok(ModelChoice {
                provider: data
                    .get("provider")
                    .and_then(Value::as_str)
                    .unwrap_or(&provider)
                    .to_owned(),
                id: data
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(&model_id)
                    .to_owned(),
                name: data.get("name").and_then(Value::as_str).map(str::to_owned),
            })
        })
    }

    fn get_available_models(
        &self,
        session_id: &str,
    ) -> BoxFuture<Result<Vec<ModelChoice>, String>> {
        let socket = self.socket.clone();
        let transport = self.transport.clone();
        let sid = session_id.to_owned();
        Box::pin(async move {
            let frame = transport
                .request(
                    &socket,
                    json!({ "type": "get_available_models", "sessionId": sid, "observe": true }),
                    None,
                )
                .await?;
            let data = data_record(&frame, "get_available_models")?;
            let models = data
                .get("models")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(models
                .into_iter()
                .filter_map(|m| {
                    let obj = m.as_object()?;
                    Some(ModelChoice {
                        provider: obj.get("provider")?.as_str()?.to_owned(),
                        id: obj.get("id")?.as_str()?.to_owned(),
                        name: obj.get("name").and_then(Value::as_str).map(str::to_owned),
                    })
                })
                .collect())
        })
    }

    fn set_thinking_level(
        &self,
        session_id: &str,
        level: &str,
        scope: Option<&str>,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<(), String>> {
        let socket = self.socket.clone();
        let transport = self.transport.clone();
        let sid = session_id.to_owned();
        let level = level.to_owned();
        let scope = scope.map(str::to_owned);
        let did = durable_session_id.map(str::to_owned);
        Box::pin(async move {
            let mut cmd = json!({
                "type": "set_thinking_level",
                "sessionId": sid,
                "level": level,
            });
            if scope.as_deref() == Some("turn")
                && let Some(obj) = cmd.as_object_mut()
            {
                obj.insert("scope".into(), Value::String("turn".into()));
            }
            let frame = transport.request(&socket, cmd, did).await?;
            data_record(&frame, "set_thinking_level").map(|_| ())
        })
    }

    fn get_available_thinking_levels(
        &self,
        session_id: &str,
    ) -> BoxFuture<Result<Vec<String>, String>> {
        let socket = self.socket.clone();
        let transport = self.transport.clone();
        let sid = session_id.to_owned();
        Box::pin(async move {
            let frame = transport
                .request(
                    &socket,
                    json!({ "type": "get_available_thinking_levels", "sessionId": sid, "observe": true }),
                    None,
                )
                .await?;
            let data = data_record(&frame, "get_available_thinking_levels")?;
            Ok(data
                .get("levels")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default())
        })
    }

    fn close_session(
        &self,
        session_id: &str,
        durable_session_id: Option<&str>,
        options: ThreadCloseOptions,
    ) -> BoxFuture<Result<ThreadCloseAnswer, String>> {
        let socket = self.socket.clone();
        let transport = self.transport.clone();
        let key_root = self.agent_dir.clone();
        let sid = session_id.to_owned();
        let did = durable_session_id.map(str::to_owned);
        Box::pin(async move {
            let send: super::control::close_ladder::LadderSend = Box::new(move |rpc, wait_ms| {
                let socket = socket.clone();
                let transport = transport.clone();
                let key_root = key_root.clone();
                let did = did.clone();
                Box::pin(async move {
                    let cmd_type = rpc
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let frame_res = if let Some(w) = wait_ms {
                        request_frame(
                            &socket,
                            Value::Object(rpc),
                            RpcCallOptions {
                                timeout_ms: Some(w.saturating_add(LADDER_ANSWER_MARGIN_MS)),
                                key_root,
                                durable_session_id: did,
                                rpc_host: !is_tui_control_socket(&socket),
                                ..Default::default()
                            },
                        )
                        .await
                    } else if cmd_type == "get_state" {
                        request_frame(
                            &socket,
                            Value::Object(rpc),
                            RpcCallOptions {
                                key_root,
                                durable_session_id: did,
                                rpc_host: !is_tui_control_socket(&socket),
                                attached: true,
                                ..Default::default()
                            },
                        )
                        .await
                    } else if cmd_type == "get_protocol_info" {
                        transport
                            .request(
                                &socket,
                                json!({ "type": "get_protocol_info", "observe": true }),
                                None,
                            )
                            .await
                    } else {
                        transport.request(&socket, Value::Object(rpc), did).await
                    };

                    match frame_res {
                        Ok(frame) => match data_record(&frame, &cmd_type) {
                            Ok(data) => Ok(RecordAnswer::Ok(data)),
                            Err(err) => {
                                let reason = err.split(':').next().unwrap_or("");
                                if is_session_gone(reason) {
                                    Ok(RecordAnswer::Gone {
                                        reason: reason.to_owned(),
                                    })
                                } else {
                                    Err(ControlError::Transport(err))
                                }
                            }
                        },
                        Err(err) => {
                            let reason = err.split(':').next().unwrap_or("");
                            if is_session_gone(reason) {
                                Ok(RecordAnswer::Gone {
                                    reason: reason.to_owned(),
                                })
                            } else {
                                Err(ControlError::Transport(err))
                            }
                        }
                    }
                })
            });
            close_ladder(&sid, send, options, None)
                .await
                .map_err(|e| match e {
                    ControlError::Transport(s) => s,
                    ControlError::Refusal(r) => r.failure.message,
                })
        })
    }

    fn watch_session(
        &self,
        session_id: &str,
        handlers: Arc<dyn SessionWatchHandlers>,
        _durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<SessionWatchOutcome, String>> {
        #[cfg(not(unix))]
        {
            let _ = (session_id, handlers);
            Box::pin(async {
                Ok(SessionWatchOutcome::Refused {
                    reason: "unsupported:subscribe_sessions".to_owned(),
                })
            })
        }
        #[cfg(unix)]
        {
            let socket_path = self.socket.clone();
            let key_root = self.agent_dir.clone();
            let sid = session_id.to_owned();
            Box::pin(async move {
                use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

                let tui = is_tui_control_socket(&socket_path);
                let mut stream = if !tui {
                    let probe = json!({ "type": "get_protocol_info", "observe": true });
                    let (sock, _) = maho_rpc::host_probe::authenticated_host_socket(
                        &socket_path,
                        &probe,
                        REQUEST_TIMEOUT_MS,
                        key_root.as_deref(),
                    )
                    .await
                    .map_err(|e| format!("host_unavailable:{socket_path}: {e}"))?;
                    sock.into_stream()
                        .map_err(|e| format!("host_unavailable:{socket_path}: {e}"))?
                } else {
                    let secret = std::fs::read(control_socket_secret_path(&socket_path))
                        .map_err(|_| format!("host_unavailable:{socket_path}"))?;
                    let mut s = tokio::net::UnixStream::connect(&socket_path)
                        .await
                        .map_err(|e| format!("host_unavailable:{socket_path}: {e}"))?;
                    s.write_all(&secret)
                        .await
                        .map_err(|e| format!("host_unavailable:{socket_path}: {e}"))?;
                    s
                };

                let req_id = uuid::Uuid::new_v4().to_string();
                let sub = json!({
                    "id": req_id,
                    "type": "subscribe_sessions",
                    "sessions": { "ids": [sid] },
                    "mirror": ["agent_start", "turn_start", "agent_settled"],
                    "observe": true,
                });
                let mut line = serde_json::to_vec(&sub)
                    .map_err(|e| format!("subscribe serialize failed: {e}"))?;
                line.push(b'\n');
                stream
                    .write_all(&line)
                    .await
                    .map_err(|e| format!("host_unavailable:{socket_path}: {e}"))?;

                let mut reader = BufReader::new(stream);
                let mut buf = Vec::new();
                let reply = loop {
                    buf.clear();
                    let n = reader
                        .read_until(b'\n', &mut buf)
                        .await
                        .map_err(|e| format!("host_unavailable:{socket_path}: {e}"))?;
                    if n == 0 {
                        return Ok(SessionWatchOutcome::Refused {
                            reason: "connection_closed".to_owned(),
                        });
                    }
                    if let Ok(frame) = serde_json::from_slice::<Value>(&buf) {
                        if frame.get("id").and_then(Value::as_str) == Some(&req_id) {
                            break frame;
                        }
                        if frame.get("sessionId").and_then(Value::as_str) == Some(&sid) {
                            handlers.record(&frame);
                        }
                    }
                };

                if reply.get("success").and_then(Value::as_bool) != Some(true) {
                    let reason = reply
                        .get("errorCode")
                        .or_else(|| reply.get("error"))
                        .and_then(Value::as_str)
                        .unwrap_or("subscribe_refused")
                        .to_owned();
                    return Ok(SessionWatchOutcome::Refused { reason });
                }

                let initial = reply
                    .get("data")
                    .and_then(|d| d.get("snapshot"))
                    .and_then(Value::as_array)
                    .and_then(|snap| {
                        snap.iter()
                            .find(|c| c.get("sessionId").and_then(Value::as_str) == Some(&sid))
                    })
                    .and_then(|row| {
                        let status = row.get("status").and_then(Value::as_str)?.to_owned();
                        Some(SessionWatchInitial {
                            status,
                            current_turn_id: row
                                .get("currentTurnId")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                        })
                    });

                let (end_tx, end_rx) = tokio::sync::oneshot::channel::<String>();
                let end_tx = Arc::new(Mutex::new(Some(end_tx)));
                let stop_notify = Arc::new(tokio::sync::Notify::new());

                let stop_n = stop_notify.clone();
                let end_t = end_tx.clone();
                let mut prev_status = initial.as_ref().map(|i| i.status.clone());
                tokio::spawn(async move {
                    let mut line_buf = Vec::new();
                    loop {
                        line_buf.clear();
                        tokio::select! {
                            () = stop_n.notified() => {
                                if let Some(tx) = end_t.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
                                    let _ = tx.send("stopped".to_owned());
                                }
                                break;
                            }
                            res = reader.read_until(b'\n', &mut line_buf) => {
                                match res {
                                    Ok(0) | Err(_) => {
                                        if let Some(tx) = end_t.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
                                            let _ = tx.send("connection_closed".to_owned());
                                        }
                                        break;
                                    }
                                    Ok(_) => {
                                        let Ok(entry) = serde_json::from_slice::<Value>(&line_buf) else {
                                            continue;
                                        };
                                        if entry.get("sessionId").and_then(Value::as_str) != Some(&sid) {
                                            continue;
                                        }
                                        if entry.get("type").and_then(Value::as_str) == Some("session_status") {
                                            if let Some(to) = entry.get("status").and_then(Value::as_str) {
                                                let cause = entry
                                                    .get("lastChange")
                                                    .and_then(|c| c.get("cause"))
                                                    .and_then(Value::as_str)
                                                    .map(str::to_owned);
                                                handlers.status(SessionWatchStatusChange {
                                                    from: prev_status.take(),
                                                    to: to.to_owned(),
                                                    cause,
                                                });
                                                prev_status = Some(to.to_owned());
                                                if to == "closed" {
                                                    if let Some(tx) = end_t.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
                                                        let _ = tx.send("session_closed".to_owned());
                                                    }
                                                    break;
                                                }
                                            }
                                        } else {
                                            handlers.record(&entry);
                                        }
                                    }
                                }
                            }
                        }
                    }
                });

                struct LiveWatch {
                    initial: Option<SessionWatchInitial>,
                    end_rx: Mutex<Option<tokio::sync::oneshot::Receiver<String>>>,
                    end_tx: Arc<Mutex<Option<tokio::sync::oneshot::Sender<String>>>>,
                    stop_notify: Arc<tokio::sync::Notify>,
                }

                impl SessionWatch for LiveWatch {
                    fn initial(&self) -> Option<SessionWatchInitial> {
                        self.initial.clone()
                    }
                    fn ended(&self) -> BoxFuture<String> {
                        let rx = self
                            .end_rx
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take();
                        Box::pin(async move {
                            match rx {
                                Some(r) => {
                                    r.await.unwrap_or_else(|_| "connection_closed".to_owned())
                                }
                                None => "stopped".to_owned(),
                            }
                        })
                    }
                    fn stop(&self) {
                        if let Some(tx) = self
                            .end_tx
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take()
                        {
                            let _ = tx.send("stopped".to_owned());
                        }
                        self.stop_notify.notify_waiters();
                    }
                }

                Ok(SessionWatchOutcome::Ok(Arc::new(LiveWatch {
                    initial,
                    end_rx: Mutex::new(Some(end_rx)),
                    end_tx,
                    stop_notify,
                })))
            })
        }
    }
}

impl ThreadSessionPort for LiveThreadSurface {
    fn get_messages(
        &self,
        session_id: &str,
    ) -> BoxFuture<Result<Vec<ThreadTranscriptEntry>, String>> {
        self.endpoint_port(self.legacy.clone())
            .get_messages(session_id)
    }
    fn get_state(
        &self,
        session_id: &str,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<SessionStateSnapshot, String>> {
        self.endpoint_port(self.legacy.clone())
            .get_state(session_id, durable_session_id)
    }
    fn prompt(
        &self,
        session_id: &str,
        message: &str,
        options: Option<PromptDeliveryOptions>,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Option<String>, String>> {
        self.endpoint_port(self.legacy.clone()).prompt(
            session_id,
            message,
            options,
            durable_session_id,
        )
    }
    fn interrupt(
        &self,
        session_id: &str,
        request: InterruptRequest,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Value, String>> {
        self.endpoint_port(self.legacy.clone())
            .interrupt(session_id, request, durable_session_id)
    }
    fn set_session_name(
        &self,
        session_id: &str,
        name: &str,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<(), String>> {
        self.endpoint_port(self.legacy.clone()).set_session_name(
            session_id,
            name,
            durable_session_id,
        )
    }
    fn set_session_archived(
        &self,
        session_id: &str,
        archived: bool,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<(), String>> {
        self.endpoint_port(self.legacy.clone())
            .set_session_archived(session_id, archived, durable_session_id)
    }
    fn set_model(
        &self,
        session_id: &str,
        provider: &str,
        model_id: &str,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<ModelChoice, String>> {
        self.endpoint_port(self.legacy.clone()).set_model(
            session_id,
            provider,
            model_id,
            durable_session_id,
        )
    }
    fn get_available_models(
        &self,
        session_id: &str,
    ) -> BoxFuture<Result<Vec<ModelChoice>, String>> {
        self.endpoint_port(self.legacy.clone())
            .get_available_models(session_id)
    }
    fn set_thinking_level(
        &self,
        session_id: &str,
        level: &str,
        scope: Option<&str>,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<(), String>> {
        self.endpoint_port(self.legacy.clone()).set_thinking_level(
            session_id,
            level,
            scope,
            durable_session_id,
        )
    }
    fn get_available_thinking_levels(
        &self,
        session_id: &str,
    ) -> BoxFuture<Result<Vec<String>, String>> {
        self.endpoint_port(self.legacy.clone())
            .get_available_thinking_levels(session_id)
    }
    fn close_session(
        &self,
        session_id: &str,
        durable_session_id: Option<&str>,
        options: ThreadCloseOptions,
    ) -> BoxFuture<Result<ThreadCloseAnswer, String>> {
        self.endpoint_port(self.legacy.clone()).close_session(
            session_id,
            durable_session_id,
            options,
        )
    }
    fn watch_session(
        &self,
        session_id: &str,
        handlers: Arc<dyn SessionWatchHandlers>,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<SessionWatchOutcome, String>> {
        self.endpoint_port(self.legacy.clone()).watch_session(
            session_id,
            handlers,
            durable_session_id,
        )
    }
}

impl ThreadHost for LiveThreadSurface {
    fn socket(&self) -> &str {
        &self.legacy
    }

    fn list_sessions(&self) -> BoxFuture<Result<Vec<ThreadHostSession>, String>> {
        let fut = self.list_view(ThreadHostViewRequest { offline: false });
        Box::pin(async move { Ok(fut.await?.unwrap_or_default().sessions) })
    }

    fn open_session(
        &self,
        params: OpenSessionParams,
    ) -> BoxFuture<Result<ThreadHostSession, String>> {
        let mut obj = Map::new();
        if let Some(cwd) = params.cwd {
            obj.insert("cwd".into(), Value::String(cwd));
        }
        if let Some(sp) = params.session_path {
            obj.insert("sessionPath".into(), Value::String(sp));
        }
        if let Some(ff) = params.fork_from {
            obj.insert("forkFrom".into(), Value::String(ff));
        }
        if let Some(pp) = params.permission_preset {
            obj.insert("permissionPreset".into(), Value::String(pp));
        }
        if let Some(p) = params.provider {
            obj.insert("provider".into(), Value::String(p));
        }
        if let Some(m) = params.model_id {
            obj.insert("modelId".into(), Value::String(m));
        }
        if let Some(tl) = params.thinking_level {
            obj.insert("thinkingLevel".into(), Value::String(tl));
        }
        if let Some(did) = params.durable_session_id {
            obj.insert("durableSessionId".into(), Value::String(did));
        }
        if let Some(skills) = params.initial_skills {
            let encoded = serde_json::to_string(&skills).unwrap_or_else(|_| "[]".to_owned());
            obj.insert(
                "context".into(),
                json!({ INITIAL_SKILLS_CONTEXT_KEY: encoded }),
            );
        }
        let name = params.name;
        let legacy = self.legacy.clone();
        let fut = self.open_session(Value::Object(obj), name.as_deref());
        Box::pin(async move {
            let val = fut.await?;
            let mut session = Self::parse_host_session(
                &val,
                &legacy,
                EndpointKind::RpcHost,
                &accepted_controls(EndpointKind::RpcHost, None),
                None,
            )
            .ok_or_else(|| "open_session reply missing sessionId".to_owned())?;
            if session.name.is_none() {
                session.name = name;
            }
            Ok(session)
        })
    }

    fn list_view(
        &self,
        request: ThreadHostViewRequest,
    ) -> BoxFuture<Result<Option<ThreadHostView>, String>> {
        let endpoints = self.known_endpoints();
        let transport = self.transport.clone();
        let legacy = self.legacy.clone();
        let known_kinds: BTreeMap<String, GatewayEndpointKind> =
            self.kinds.lock().map(|g| g.clone()).unwrap_or_default();
        let previous_paths: BTreeMap<String, Vec<String>> = self
            .last_listed
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();

        Box::pin(async move {
            let mut all_sessions = Vec::new();
            let mut all_hosts = Vec::new();
            let mut all_disk = Vec::new();
            let mut first_error: Option<String> = None;
            let mut any_ok = false;

            for ep in endpoints {
                let kind_gw = ep
                    .kind
                    .or_else(|| known_kinds.get(&ep.socket).copied())
                    .unwrap_or_else(|| {
                        if is_tui_control_socket(&ep.socket) {
                            GatewayEndpointKind::Tui
                        } else {
                            GatewayEndpointKind::RpcHost
                        }
                    });
                let kind = match kind_gw {
                    GatewayEndpointKind::Tui => EndpointKind::Tui,
                    GatewayEndpointKind::RpcHost => EndpointKind::RpcHost,
                };
                if kind == EndpointKind::Tui && ep.alive == Some(false) {
                    let err_str = ep
                        .reason
                        .clone()
                        .unwrap_or_else(|| "live_unresponsive".to_owned());
                    if first_error.is_none() {
                        first_error = Some(err_str.clone());
                    }
                    let mut paths: HashSet<String> = ep.paths.into_iter().collect();
                    if let Some(prev) = previous_paths.get(&ep.socket) {
                        paths.extend(prev.iter().cloned());
                    }
                    for p in paths {
                        if let Some(ds) = read_disk_session(Path::new(&p), Some(&ep.socket)) {
                            all_disk.push(ds);
                        }
                    }
                    all_hosts.push(AddressBookHost {
                        socket: ep.socket.clone(),
                        list_sessions: None,
                        error: Some(err_str),
                        endpoint_kind: kind_gw,
                        alive: Some(false),
                        reason: ep.reason,
                        legacy: ep.socket == legacy,
                    });
                    continue;
                }

                let list_cmd = if kind == EndpointKind::Tui {
                    json!({ "type": "list_sessions" })
                } else {
                    json!({ "type": "list_sessions", "observe": true })
                };
                match transport.request(&ep.socket, list_cmd, None).await {
                    Ok(frame) => match data_record(&frame, "list_sessions") {
                        Ok(data) => {
                            any_ok = true;
                            let commands = if kind == EndpointKind::Tui {
                                match transport
                                    .request(
                                        &ep.socket,
                                        json!({ "type": "get_protocol_info" }),
                                        None,
                                    )
                                    .await
                                {
                                    Ok(info) => Some(tui_commands_from(&info)),
                                    Err(_) => Some(legacy_tui_commands()),
                                }
                            } else {
                                None
                            };
                            let controls = accepted_controls(kind, commands.as_ref());
                            let raw_sessions = data
                                .get("sessions")
                                .and_then(Value::as_array)
                                .cloned()
                                .unwrap_or_default();
                            let mut parsed_sessions = Vec::new();
                            let mut host_sessions = Vec::new();
                            for raw in &raw_sessions {
                                if let Some(ts) =
                                    Self::parse_host_session(raw, &ep.socket, kind, &controls, None)
                                {
                                    host_sessions.push(HostSession {
                                        session_id: ts.session_id.clone(),
                                        durable_session_id: ts.durable_session_id.clone(),
                                        session_path: ts.session_path.clone(),
                                        cwd: ts.cwd.clone(),
                                        name: ts.name.clone(),
                                        status: ts
                                            .status
                                            .as_deref()
                                            .and_then(HostSessionStatus::parse),
                                        kind: ts.kind.clone(),
                                        created_at: ts.created_at.clone(),
                                        updated_at: ts.updated_at.clone(),
                                    });
                                    parsed_sessions.push(ts);
                                }
                            }
                            all_sessions.extend(parsed_sessions);
                            all_hosts.push(AddressBookHost {
                                socket: ep.socket.clone(),
                                list_sessions: Some(HostListSessions::ok(host_sessions)),
                                error: None,
                                endpoint_kind: kind_gw,
                                alive: Some(true),
                                reason: None,
                                legacy: ep.socket == legacy,
                            });
                        }
                        Err(err) => {
                            if first_error.is_none() {
                                first_error = Some(err.clone());
                            }
                            let mut paths: HashSet<String> = ep.paths.into_iter().collect();
                            if let Some(prev) = previous_paths.get(&ep.socket) {
                                paths.extend(prev.iter().cloned());
                            }
                            for p in paths {
                                if let Some(ds) = read_disk_session(Path::new(&p), Some(&ep.socket))
                                {
                                    all_disk.push(ds);
                                }
                            }
                            all_hosts.push(AddressBookHost {
                                socket: ep.socket.clone(),
                                list_sessions: None,
                                error: Some(err),
                                endpoint_kind: kind_gw,
                                alive: Some(false),
                                reason: ep.reason,
                                legacy: ep.socket == legacy,
                            });
                        }
                    },
                    Err(err) => {
                        if first_error.is_none() {
                            first_error = Some(err.clone());
                        }
                        let mut paths: HashSet<String> = ep.paths.into_iter().collect();
                        if let Some(prev) = previous_paths.get(&ep.socket) {
                            paths.extend(prev.iter().cloned());
                        }
                        for p in paths {
                            if let Some(ds) = read_disk_session(Path::new(&p), Some(&ep.socket)) {
                                all_disk.push(ds);
                            }
                        }
                        all_hosts.push(AddressBookHost {
                            socket: ep.socket.clone(),
                            list_sessions: None,
                            error: Some(err),
                            endpoint_kind: kind_gw,
                            alive: Some(false),
                            reason: ep.reason,
                            legacy: ep.socket == legacy,
                        });
                    }
                }
            }

            if !request.offline
                && !any_ok
                && let Some(err) = first_error
            {
                return Err(err);
            }

            Ok(Some(ThreadHostView {
                sessions: all_sessions,
                hosts: all_hosts,
                disk: all_disk,
            }))
        })
    }

    fn list_target(
        &self,
        durable_id: &str,
        endpoint_socket: &str,
        endpoint_kind: EndpointKind,
    ) -> BoxFuture<Result<Option<ThreadHostView>, String>> {
        let ep_ref = GatewayEndpointRef {
            kind: to_gateway_endpoint_kind(endpoint_kind),
            socket: endpoint_socket.to_owned(),
            routing_id: None,
            durable_session_id: durable_id.to_owned(),
        };
        self.set_kind(endpoint_socket, ep_ref.kind);
        let legacy = self.legacy.clone();
        let socket = endpoint_socket.to_owned();
        let transport = self.transport.clone();
        let did = durable_id.to_owned();
        Box::pin(async move {
            let list_cmd = if endpoint_kind == EndpointKind::Tui {
                json!({ "type": "list_sessions" })
            } else {
                json!({ "type": "list_sessions", "observe": true })
            };
            match transport.request(&socket, list_cmd, None).await {
                Ok(frame) => {
                    let data = data_record(&frame, "list_sessions")?;
                    let controls = accepted_controls(endpoint_kind, None);
                    let raw_sessions = data
                        .get("sessions")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let mut parsed = Vec::new();
                    let mut host_sessions = Vec::new();
                    for raw in &raw_sessions {
                        if let Some(ts) =
                            Self::parse_host_session(raw, &socket, endpoint_kind, &controls, None)
                            && ts.durable_session_id.as_deref().unwrap_or(&ts.session_id) == did
                            && ts.status.as_deref() != Some("closed")
                        {
                            host_sessions.push(HostSession {
                                session_id: ts.session_id.clone(),
                                durable_session_id: ts.durable_session_id.clone(),
                                session_path: ts.session_path.clone(),
                                cwd: ts.cwd.clone(),
                                name: ts.name.clone(),
                                status: ts.status.as_deref().and_then(HostSessionStatus::parse),
                                kind: ts.kind.clone(),
                                created_at: ts.created_at.clone(),
                                updated_at: ts.updated_at.clone(),
                            });
                            parsed.push(ts);
                        }
                    }
                    let host = AddressBookHost {
                        socket: socket.clone(),
                        list_sessions: Some(HostListSessions::ok(host_sessions)),
                        error: None,
                        endpoint_kind: ep_ref.kind,
                        alive: Some(true),
                        reason: None,
                        legacy: socket == legacy,
                    };
                    Ok(Some(ThreadHostView {
                        sessions: parsed,
                        hosts: vec![host],
                        disk: Vec::new(),
                    }))
                }
                Err(err) => {
                    let host = AddressBookHost {
                        socket: socket.clone(),
                        list_sessions: None,
                        error: Some(err),
                        endpoint_kind: ep_ref.kind,
                        alive: Some(false),
                        reason: None,
                        legacy: socket == legacy,
                    };
                    Ok(Some(ThreadHostView {
                        sessions: Vec::new(),
                        hosts: vec![host],
                        disk: Vec::new(),
                    }))
                }
            }
        })
    }

    fn endpoint(&self, socket: &str) -> Option<Arc<dyn ThreadSessionPort>> {
        Some(self.endpoint_port(socket.to_owned()))
    }

    fn gateway(&self) -> Option<GatewayEndpointPort> {
        Some(LiveThreadSurface::gateway(self))
    }
}

/// The parts a gateway port needs after the surface is boxed.
struct SurfaceHandle {
    transport: Arc<dyn RpcTransport>,
}

impl SurfaceHandle {
    fn fenced(
        &self,
        socket: &str,
        command: Value,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Value, String>> {
        let socket = socket.to_owned();
        let transport = self.transport.clone();
        let durable = durable_session_id.map(str::to_owned);
        let capabilities: BTreeSet<String> = [
            "attach_session",
            "release_attachment",
            "release_caller_aware",
        ]
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
        Box::pin(async move {
            let send_command = command.clone();
            let send_transport = transport.clone();
            let send_socket = socket.clone();
            let send_durable = durable.clone();
            let send = move || {
                send_transport.request(&send_socket, send_command.clone(), send_durable.clone())
            };
            match fenced_command(
                transport.as_ref(),
                &socket,
                command,
                &capabilities,
                durable.as_deref(),
                &send,
            )
            .await?
            {
                FenceOutcome::Ran(frame) | FenceOutcome::Refused(frame) => Ok(frame),
                FenceOutcome::Failed(error) => Err(error),
            }
        })
    }

    fn call_on(
        &self,
        socket: &str,
        command: &str,
        data: Value,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Value, String>> {
        let mut frame = data;
        if let Some(object) = frame.as_object_mut() {
            object.insert("type".into(), Value::String(command.to_owned()));
        }
        let command_name = command.to_owned();
        let pending = self.fenced(socket, frame, durable_session_id);
        Box::pin(async move {
            let frame = pending.await?;
            let data = data_record(&frame, &command_name)?;
            Ok(Value::Object(data))
        })
    }

    fn routing_id_of(
        &self,
        endpoint: &GatewayEndpointRef,
    ) -> BoxFuture<Result<Option<String>, String>> {
        if let Some(routing_id) = &endpoint.routing_id {
            let routing_id = routing_id.clone();
            return Box::pin(async move { Ok(Some(routing_id)) });
        }
        let socket = endpoint.socket.clone();
        let durable_id = endpoint.durable_session_id.clone();
        let listing = self.call_on(&socket, "list_sessions", json!({ "observe": true }), None);
        Box::pin(async move {
            let data = listing.await?;
            let sessions = data
                .get("sessions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(sessions.into_iter().find_map(|session| {
                let durable = session.get("durableSessionId").and_then(Value::as_str);
                (durable == Some(durable_id.as_str())
                    && session.get("status").and_then(Value::as_str) != Some("closed"))
                .then(|| {
                    session
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .flatten()
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_last_json_line_of_host_status_all_is_read() {
        let stdout = "warning: legacy engine\n{\"endpoints\":[{\"socket\":\"/run/i-1.sock\",\"reachable\":true,\"session_rows\":[{\"session_path\":\"/s/a.jsonl\"}],\"endpoint_kind\":\"rpc_host\",\"alive\":true}]}\n";
        let reports = parse_host_status_all(stdout).expect("reports");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].socket.as_deref(), Some("/run/i-1.sock"));
        assert_eq!(reports[0].session_paths, vec!["/s/a.jsonl".to_owned()]);
        assert!(parse_host_status_all("not json").is_none());
        assert!(parse_host_status_all("{\"endpoints\": 3}").is_none());
    }

    #[test]
    fn an_unsupported_command_and_an_unsupported_thinking_level_keep_their_codes() {
        assert_eq!(
            data_record(
                &json!({ "success": false, "error": "unsupported" }),
                "prompt"
            ),
            Err("unsupported:prompt".to_owned())
        );
        assert_eq!(
            data_record(
                &json!({ "success": false, "error": "Thinking level max is not supported by the active model." }),
                "set_thinking_level"
            ),
            Err("thinking_level_unsupported:Thinking level max is not supported by the active model.".to_owned())
        );
        assert!(data_record(&json!({ "success": true }), "set_session_name").is_ok());
        assert!(data_record(&json!({ "success": true }), "close_session").is_ok());
        assert!(data_record(&json!({ "success": true }), "prompt").is_ok());
        assert!(data_record(&json!({ "success": true }), "interrupt").is_ok());
        assert_eq!(
            data_record(
                &json!({ "success": false, "errorCode": "unknown_command" }),
                "set_session_archived"
            ),
            Err("unsupported:set_session_archived".to_owned())
        );
    }
}
