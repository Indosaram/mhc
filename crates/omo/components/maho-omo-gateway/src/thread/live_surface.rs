//! The thread tools' client for every endpoint an agent directory holds.
//!
//! Upstream counterpart: `thread/live-surface.ts`. The wire client is the `RpcTransport` seam: this
//! module owns the surface logic (which endpoint is dialed, which session a command names, which
//! refusal a reply means), and the transport owns the socket.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};

use super::adapter::{BoxFuture, EndpointLiveness, GatewayEndpointPort, GatewayEndpointRef, ReleaseSessionReply, UiAnswer, UiAnswerReply};
use super::attachment_fence::{FenceOutcome, RpcTransport, fenced_command};
use super::gateway_replies::{release_reply, wake_reply};
use super::types::GatewayEndpointKind;

pub const REQUEST_TIMEOUT_MS: u64 = 60_000;
/// Budget for one endpoint's `list_sessions` while enumerating: one hung shard must not stall every call.
pub const ENDPOINT_LIST_TIMEOUT_MS: u64 = 10_000;
/// A terminal answers from the user's own process: a suspended or busy one costs a listing at most this long.
pub const TUI_REQUEST_TIMEOUT_MS: u64 = 1_500;
/// How long one `host status --all` enumeration is reused before the engine is asked again.
pub const HOST_ENDPOINTS_CACHE_TTL_MS: i64 = 5_000;
pub const HOST_STATUS_ALL_ARGS: [&str; 5] = ["host", "status", "--all", "--include-workers", "--json"];

/// The commands any terminal control endpoint answers, an older one included.
pub const LEGACY_TUI_COMMANDS: [&str; 5] = ["list_sessions", "get_messages", "get_state", "get_protocol_info", "release_session"];

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
        .map(|rows| rows.iter().filter_map(|row| row.get(field)?.as_str().map(str::to_owned)).collect())
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
    let line = stdout.lines().map(str::trim).rfind(|line| !line.is_empty())?;
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
                    socket: endpoint.get("socket").and_then(Value::as_str).filter(|socket| !socket.is_empty()).map(str::to_owned),
                    reachable: endpoint.get("reachable").and_then(Value::as_bool).unwrap_or(false),
                    session_paths,
                    endpoint_kind: match endpoint.get("endpoint_kind").and_then(Value::as_str) {
                        Some("tui") => Some(GatewayEndpointKind::Tui),
                        Some("rpc_host") => Some(GatewayEndpointKind::RpcHost),
                        _ => None,
                    },
                    alive: endpoint.get("alive").and_then(Value::as_bool),
                    reason: endpoint.get("reason").and_then(Value::as_str).map(str::to_owned),
                })
            })
            .collect(),
    )
}

/// The frame a command's `data`, or the failure its reply names.
pub fn data_record(frame: &Value, command: &str) -> Result<Map<String, Value>, String> {
    let success = frame.get("success").and_then(Value::as_bool);
    let error = frame.get("error").and_then(Value::as_str);
    if success == Some(false)
        && command == "set_thinking_level"
        && error.is_some_and(|error| error.starts_with("Thinking level ") && error.ends_with(" is not supported by the active model."))
    {
        return Err(format!("thinking_level_unsupported:{}", error.unwrap_or_default()));
    }
    if success == Some(false) && error == Some("unsupported") {
        return Err(format!("unsupported:{command}"));
    }
    if success == Some(true)
        && frame.get("data").is_none()
        && matches!(command, "set_session_name" | "set_thinking_level")
    {
        return Ok(Map::new());
    }
    match frame.get("data").and_then(Value::as_object) {
        Some(data) if success == Some(true) => Ok(data.clone()),
        _ => Err(format!("thread RPC request failed: {}", frame.get("error").map_or_else(|| frame.to_string(), Value::to_string))),
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
    transport: Arc<dyn RpcTransport>,
    status_all: Arc<dyn Fn() -> Option<Vec<HostEndpointReport>> + Send + Sync>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    enumeration: Mutex<Option<(i64, Vec<KnownEndpoint>)>>,
    kinds: Mutex<BTreeMap<String, GatewayEndpointKind>>,
}

impl LiveThreadSurface {
    pub fn new(options: LiveThreadSurfaceOptions) -> Self {
        Self {
            legacy: options.socket,
            transport: options.transport,
            status_all: options.status_all.unwrap_or_else(|| Arc::new(|| None)),
            now: options.now.unwrap_or_else(|| Arc::new(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_millis() as i64).unwrap_or(0))),
            enumeration: Mutex::new(None),
            kinds: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn socket(&self) -> &str {
        &self.legacy
    }

    fn kind_of(&self, socket: &str) -> GatewayEndpointKind {
        self.kinds.lock().ok().and_then(|kinds| kinds.get(socket).copied()).unwrap_or(GatewayEndpointKind::RpcHost)
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
                        Some(KnownEndpoint { socket: report.socket?, paths: report.session_paths, kind: report.endpoint_kind, alive: report.alive, reason: report.reason })
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !found.iter().any(|endpoint| endpoint.socket == self.legacy) {
            found.insert(0, KnownEndpoint { socket: self.legacy.clone(), paths: Vec::new(), kind: None, alive: None, reason: None });
        }
        if let Ok(mut cached) = self.enumeration.lock() {
            *cached = Some(((*self.now)() + HOST_ENDPOINTS_CACHE_TTL_MS, found.clone()));
        }
        found
    }

    pub fn call(&self, socket: &str, command: Value, durable_session_id: Option<&str>) -> BoxFuture<Result<Value, String>> {
        self.transport.request(socket, command, durable_session_id.map(str::to_owned))
    }

    /// One request under the attachment fence, with a terminal's secret and command set honored.
    fn fenced(&self, socket: &str, command: Value, durable_session_id: Option<&str>) -> BoxFuture<Result<Value, String>> {
        let socket = socket.to_owned();
        let kind = self.kind_of(&socket);
        let transport = self.transport.clone();
        let durable = durable_session_id.map(str::to_owned);
        let capabilities: std::collections::BTreeSet<String> = ["attach_session", "release_attachment", "release_caller_aware"].iter().map(|name| (*name).to_owned()).collect();
        Box::pin(async move {
            if kind == GatewayEndpointKind::Tui {
                let command_type = command.get("type").and_then(Value::as_str).unwrap_or_default().to_owned();
                if !LEGACY_TUI_COMMANDS.contains(&command_type.as_str()) {
                    return Err(format!("unsupported:{command_type}"));
                }
                return transport.request(&socket, command, None).await;
            }
            let send_command = command.clone();
            let send_transport = transport.clone();
            let send_socket = socket.clone();
            let send = move || send_transport.request(&send_socket, send_command.clone(), None);
            match fenced_command(transport.as_ref(), &socket, command, &capabilities, durable.as_deref(), &send).await? {
                FenceOutcome::Ran(frame) | FenceOutcome::Refused(frame) => Ok(frame),
                FenceOutcome::Failed(error) => Err(error),
            }
        })
    }

    fn call_on(&self, socket: &str, command: &str, data: Value, durable_session_id: Option<&str>) -> BoxFuture<Result<Value, String>> {
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
    pub fn routing_id_of(&self, endpoint: &GatewayEndpointRef) -> BoxFuture<Result<Option<String>, String>> {
        if let Some(routing_id) = &endpoint.routing_id {
            let routing_id = routing_id.clone();
            return Box::pin(async move { Ok(Some(routing_id)) });
        }
        let socket = endpoint.socket.clone();
        let durable_id = endpoint.durable_session_id.clone();
        let listing = self.call_on(&socket, "list_sessions", serde_json::json!({ "observe": true }), None);
        Box::pin(async move {
            let data = listing.await?;
            let sessions = data.get("sessions").and_then(Value::as_array).cloned().unwrap_or_default();
            Ok(sessions.into_iter().find_map(|session| {
                let durable = session.get("durableSessionId").and_then(Value::as_str);
                let status = session.get("status").and_then(Value::as_str);
                (durable == Some(durable_id.as_str()) && status != Some("closed")).then(|| session.get("sessionId").and_then(Value::as_str).map(str::to_owned)).flatten()
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
                    let session_id = if endpoint.kind == GatewayEndpointKind::RpcHost { surface.routing_id_of(&endpoint).await.ok().flatten() } else { None };
                    if endpoint.kind == GatewayEndpointKind::RpcHost && session_id.is_none() {
                        return super::adapter::GatewayWakeReply::unroutable_no_live_session();
                    }
                    let mut command = serde_json::json!({ "type": "wake", "delivery_ids": delivery_ids });
                    if let Some(session_id) = session_id
                        && let Some(object) = command.as_object_mut()
                    {
                        object.insert("sessionId".into(), Value::String(session_id));
                    }
                    match surface.fenced(&endpoint.socket, command, Some(&endpoint.durable_session_id)).await {
                        Ok(frame) => wake_reply(&frame),
                        Err(_) => super::adapter::GatewayWakeReply::unroutable_no_live_session(),
                    }
                })
            }),
            release_session: Some(Arc::new(move |endpoint, request| {
                let surface = release_surface.clone();
                Box::pin(async move {
                    if endpoint.kind != GatewayEndpointKind::RpcHost {
                        return ReleaseSessionReply::Failure { error: "unsupported".to_owned(), error_data: None };
                    }
                    let session_id = match surface.routing_id_of(&endpoint).await {
                        Ok(Some(session_id)) => session_id,
                        _ => return ReleaseSessionReply::Failure { error: "unknown_session".to_owned(), error_data: None },
                    };
                    let command = serde_json::json!({ "type": "release_session", "sessionId": session_id, "reason": request.reason, "interrupt": request.interrupt, "force": request.force });
                    match surface.fenced(&endpoint.socket, command, Some(&endpoint.durable_session_id)).await {
                        Ok(frame) => release_reply(&frame),
                        Err(error) => ReleaseSessionReply::Failure { error, error_data: None },
                    }
                })
            })),
            classify_liveness: Some(Arc::new(move |endpoint| {
                let surface = liveness_surface.clone();
                Box::pin(async move {
                    match surface.call_on(&endpoint.socket, "get_protocol_info", serde_json::json!({ "observe": true }), None).await {
                        Ok(_) => EndpointLiveness::Routable,
                        Err(error) if error.starts_with("host_unavailable:") => EndpointLiveness::Dead,
                        Err(_) => EndpointLiveness::LiveUnresponsive,
                    }
                })
            })),
            respond_ui: Some(Arc::new(move |endpoint, answer: UiAnswer| {
                let surface = respond_surface.clone();
                Box::pin(async move {
                    let session_id = if endpoint.kind == GatewayEndpointKind::RpcHost { surface.routing_id_of(&endpoint).await.ok().flatten() } else { endpoint.routing_id.clone() };
                    if endpoint.kind == GatewayEndpointKind::RpcHost && session_id.is_none() {
                        return UiAnswerReply::Refused { error: "unknown_session".to_owned() };
                    }
                    let command = serde_json::json!({
                        "type": "extension_ui_response",
                        "sessionId": session_id,
                        "uiRequestId": answer.ui_request_id,
                        "fields": answer.fields,
                    });
                    match surface.fenced(&endpoint.socket, command, Some(&endpoint.durable_session_id)).await {
                        Ok(frame) if frame.get("success").and_then(Value::as_bool) == Some(true) => UiAnswerReply::Delivered,
                        Ok(frame) => UiAnswerReply::Refused { error: frame.get("error").and_then(Value::as_str).unwrap_or("invalid_response").to_owned() },
                        Err(error) => UiAnswerReply::Refused { error },
                    }
                })
            })),
        }
    }

    /// The owner of one durable id, listed exactly as a listing lists it.
    pub fn list_target(&self, durable_id: &str, endpoint: &GatewayEndpointRef) -> BoxFuture<Result<Vec<Value>, String>> {
        self.set_kind(&endpoint.socket, endpoint.kind);
        let socket = endpoint.socket.clone();
        let durable_id = durable_id.to_owned();
        let listing = self.call_on(&socket, "list_sessions", serde_json::json!({ "observe": true }), None);
        Box::pin(async move {
            let data = listing.await?;
            let sessions = data.get("sessions").and_then(Value::as_array).cloned().unwrap_or_default();
            Ok(sessions
                .into_iter()
                .filter(|session| {
                    let durable = session.get("durableSessionId").and_then(Value::as_str).or_else(|| session.get("sessionId").and_then(Value::as_str));
                    durable == Some(durable_id.as_str()) && session.get("status").and_then(Value::as_str) != Some("closed")
                })
                .collect())
        })
    }

    /// Opens a thread on the legacy endpoint and reads the session list back, so the reply carries the
    /// durable id and the name the address book keys entries by.
    pub fn open_session(&self, params: Value, name: Option<&str>) -> BoxFuture<Result<Value, String>> {
        let socket = self.legacy.clone();
        let name = name.map(str::to_owned);
        let transport = self.transport.clone();
        Box::pin(async move {
            let mut command = params;
            if let Some(object) = command.as_object_mut() {
                object.insert("type".into(), Value::String("open_session".into()));
                object.insert("retain_on_disconnect".into(), Value::Bool(true));
            }
            let frame = transport.request(&socket, command, None).await?;
            let mut opened = data_record(&frame, "open_session")?;
            let routing_id = opened.get("sessionId").and_then(Value::as_str).unwrap_or_default().to_owned();
            if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
                let _ = transport.request(&socket, serde_json::json!({ "type": "set_session_name", "sessionId": routing_id, "name": name }), None).await;
            }
            let listing = transport.request(&socket, serde_json::json!({ "type": "list_sessions", "observe": true }), None).await?;
            let listed = data_record(&listing, "list_sessions")?;
            let sessions = listed.get("sessions").and_then(Value::as_array).cloned().unwrap_or_default();
            if let Some(session) = sessions.into_iter().find(|session| session.get("sessionId").and_then(Value::as_str) == Some(routing_id.as_str()))
                && let Some(object) = session.as_object()
            {
                for (key, value) in object {
                    opened.insert(key.clone(), value.clone());
                }
            }
            opened.insert("sessionId".into(), Value::String(routing_id));
            opened.insert("socket".into(), Value::String(socket));
            opened.insert("endpoint_kind".into(), Value::String("rpc_host".into()));
            Ok(Value::Object(opened))
        })
    }
}

/// The parts a gateway port needs after the surface is boxed.
struct SurfaceHandle {
    transport: Arc<dyn RpcTransport>,
}

impl SurfaceHandle {
    fn fenced(&self, socket: &str, command: Value, durable_session_id: Option<&str>) -> BoxFuture<Result<Value, String>> {
        let socket = socket.to_owned();
        let transport = self.transport.clone();
        let durable = durable_session_id.map(str::to_owned);
        let capabilities: std::collections::BTreeSet<String> = ["attach_session", "release_attachment", "release_caller_aware"].iter().map(|name| (*name).to_owned()).collect();
        Box::pin(async move {
            let send_command = command.clone();
            let send_transport = transport.clone();
            let send_socket = socket.clone();
            let send = move || send_transport.request(&send_socket, send_command.clone(), None);
            match fenced_command(transport.as_ref(), &socket, command, &capabilities, durable.as_deref(), &send).await? {
                FenceOutcome::Ran(frame) | FenceOutcome::Refused(frame) => Ok(frame),
                FenceOutcome::Failed(error) => Err(error),
            }
        })
    }

    fn call_on(&self, socket: &str, command: &str, data: Value, durable_session_id: Option<&str>) -> BoxFuture<Result<Value, String>> {
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

    fn routing_id_of(&self, endpoint: &GatewayEndpointRef) -> BoxFuture<Result<Option<String>, String>> {
        if let Some(routing_id) = &endpoint.routing_id {
            let routing_id = routing_id.clone();
            return Box::pin(async move { Ok(Some(routing_id)) });
        }
        let socket = endpoint.socket.clone();
        let durable_id = endpoint.durable_session_id.clone();
        let listing = self.call_on(&socket, "list_sessions", serde_json::json!({ "observe": true }), None);
        Box::pin(async move {
            let data = listing.await?;
            let sessions = data.get("sessions").and_then(Value::as_array).cloned().unwrap_or_default();
            Ok(sessions.into_iter().find_map(|session| {
                let durable = session.get("durableSessionId").and_then(Value::as_str);
                (durable == Some(durable_id.as_str()) && session.get("status").and_then(Value::as_str) != Some("closed")).then(|| session.get("sessionId").and_then(Value::as_str).map(str::to_owned)).flatten()
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
        assert_eq!(data_record(&serde_json::json!({ "success": false, "error": "unsupported" }), "prompt"), Err("unsupported:prompt".to_owned()));
        assert_eq!(
            data_record(&serde_json::json!({ "success": false, "error": "Thinking level max is not supported by the active model." }), "set_thinking_level"),
            Err("thinking_level_unsupported:Thinking level max is not supported by the active model.".to_owned())
        );
        assert!(data_record(&serde_json::json!({ "success": true }), "set_session_name").is_ok());
    }
}
