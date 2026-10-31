//! The CloakBrowser extension bridge: Chrome native messaging between this process (the host) and
//! the omowright companion extension.
//!
//! Port of omowright `src/bridge-schemas.js` (protocol constants and the strict zod schemas, as
//! hand-written validators) and `src/bridge-transport.js`: native framing (`encodeFrame`,
//! `decodeFrames`: 4-byte little-endian length, UTF-8 JSON, size cap), the host
//! (`createNativeMessagingHost`: extension hello checked against the pinned extension id,
//! capability negotiation, one mutation in flight at a time, bounded queue, a timed-out mutation
//! disconnects because its completion is indeterminate, event ordering per connection),
//! registration (`installNativeMessagingHost`: secure directory walk, owned executable, atomic
//! `0600` manifest) and `defaultNativeMessagingHostDir`, plus the bridge-routed part of
//! `createChromeApi`.
//!
//! PORT-DIVERGENCE: requests block the calling thread (a reader thread feeds responses) instead of
//! returning promises. The `AbortSignal` option is a [`CancelToken`] passed to
//! [`NativeMessagingHost::request_cancellable`] with the same outcomes: an already-cancelled
//! token or a cancel while queued rejects with `ABORT_ERR`; a cancel after the mutation was sent
//! also disconnects, because its completion is indeterminate; a cancel after the response changes
//! nothing. `onDisconnect` is [`HostOptions::on_disconnect`], called once on the thread that
//! closed the connection, with no host lock held. Listener
//! panics are not caught (JS isolates thrown listener errors). `createChromeApi`'s standalone
//! fallbacks come from omowright `src/chrome.js`, which is not ported: unsupported calls fail with
//! the upstream "requires the extension bridge" error.
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const HOST_NAME: &str = "com.omowright.cloakbridge";
pub const PROTOCOL: u64 = 1;
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
pub const EVENT_NAMES: [&str; 8] = [
    "notifications.shown",
    "notifications.clicked",
    "notifications.closed",
    "tabGroups.created",
    "tabGroups.updated",
    "tabGroups.moved",
    "tabs.activated",
    "windows.focusChanged",
];
pub const COMMAND_NAMES: [&str; 8] = [
    "bookmarks.create",
    "bookmarks.move",
    "bookmarks.remove",
    "history.deleteUrl",
    "history.deleteRange",
    "tabGroups.update",
    "debugger.attach",
    "debugger.detach",
];
const ERROR_CODES: [&str; 9] = [
    "INVALID_MESSAGE",
    "UNKNOWN_COMMAND",
    "UNSUPPORTED_COMMAND",
    "CHROME_ERROR",
    "DEBUGGER_ERROR",
    "NATIVE_HOST_DISCONNECTED",
    "TIMEOUT",
    "PAYLOAD_TOO_LARGE",
    "INTERNAL_ERROR",
];
const COLORS: [&str; 9] = [
    "grey", "blue", "red", "yellow", "green", "pink", "purple", "cyan", "orange",
];

/// `BridgeProtocolError` / `BridgeUnavailableError` / a command error with its code.
#[derive(Clone, Debug, PartialEq)]
pub enum BridgeError {
    Protocol(String),
    Unavailable(String),
    Command {
        code: Option<String>,
        message: String,
    },
}

impl BridgeError {
    pub fn message(&self) -> &str {
        match self {
            Self::Protocol(message) | Self::Unavailable(message) => message,
            Self::Command { message, .. } => message,
        }
    }

    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Command { code, .. } => code.as_deref(),
            _ => None,
        }
    }
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

fn protocol(message: &str) -> BridgeError {
    BridgeError::Protocol(message.to_owned())
}

fn aborted(name: &str) -> BridgeError {
    BridgeError::Command {
        code: Some("ABORT_ERR".into()),
        message: format!("bridge command aborted: {name}"),
    }
}

// ---------------------------------------------------------------------------------------------
// Schemas (bridge-schemas.js)
// ---------------------------------------------------------------------------------------------

type Check = fn(&Value) -> bool;

/// A strict object: every required key passes, optional keys pass when present, nothing else.
fn strict(value: &Value, required: &[(&str, Check)], optional: &[(&str, Check)]) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let known = |key: &str| {
        required.iter().any(|(name, _)| *name == key)
            || optional.iter().any(|(name, _)| *name == key)
    };
    object.keys().all(|key| known(key))
        && required
            .iter()
            .all(|(key, check)| object.get(*key).is_some_and(check))
        && optional
            .iter()
            .all(|(key, check)| object.get(*key).is_none_or(check))
}

fn int_min(value: &Value, min: i64) -> bool {
    value
        .as_i64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|f| f.fract() == 0.0)
                .map(|f| f as i64)
        })
        .is_some_and(|n| n >= min)
}
fn int_ge0(value: &Value) -> bool {
    int_min(value, 0)
}
fn int_ge1(value: &Value) -> bool {
    int_min(value, 1)
}
fn int_ge_neg1(value: &Value) -> bool {
    int_min(value, -1)
}
fn finite_ge0(value: &Value) -> bool {
    value.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0)
}
fn boolean(value: &Value) -> bool {
    value.is_boolean()
}
fn string(value: &Value) -> bool {
    value.is_string()
}
fn nonempty(value: &Value) -> bool {
    value.as_str().is_some_and(|s| !s.is_empty())
}
fn id(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| (1..=128).contains(&s.chars().count()))
}
fn uuid_like(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        (16..=128).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
    })
}
fn text_1024(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| (1..=1024).contains(&s.chars().count()))
}
fn title_nullable(value: &Value) -> bool {
    value.is_null() || value.as_str().is_some_and(|s| s.chars().count() <= 1024)
}
fn version_64(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| (1..=64).contains(&s.chars().count()))
}
fn record(value: &Value) -> bool {
    value.is_object()
}
fn color(value: &Value) -> bool {
    value.as_str().is_some_and(|s| COLORS.contains(&s))
}
fn extension_id(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| s.len() == 32 && s.bytes().all(|b| (b'a'..=b'p').contains(&b)))
}
fn url_8192(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        s.chars().count() <= 8192
            && s.split_once(':').is_some_and(|(scheme, rest)| {
                !rest.is_empty()
                    && scheme
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphabetic())
                    && scheme
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            })
    })
}
fn group(value: &Value) -> bool {
    strict(
        value,
        &[
            ("id", int_ge0),
            ("collapsed", boolean),
            ("color", color),
            ("windowId", int_ge_neg1),
        ],
        &[("title", title_nullable)],
    )
}
fn bookmark_type(value: &Value) -> bool {
    matches!(value.as_str(), Some("bookmark" | "folder" | "url"))
}
fn bookmark_children(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|children| children.iter().all(bookmark_node))
}
/// The recursive, strict `bookmarkNode` schema.
pub fn bookmark_node(value: &Value) -> bool {
    strict(
        value,
        &[("id", nonempty), ("title", string)],
        &[
            ("parentId", nonempty),
            ("index", int_ge0),
            ("url", string),
            ("dateAdded", finite_ge0),
            ("dateGroupModified", finite_ge0),
            ("dateLastUsed", finite_ge0),
            ("folderType", string),
            ("syncing", boolean),
            ("unmodifiable", string),
            ("type", bookmark_type),
            ("children", bookmark_children),
        ],
    )
}
fn empty(value: &Value) -> bool {
    value.as_object().is_some_and(Map::is_empty)
}
fn names(value: &Value, allowed: &[&str]) -> bool {
    value.as_array().is_some_and(|items| {
        let mut seen = HashSet::new();
        items.iter().all(|item| {
            item.as_str()
                .is_some_and(|name| allowed.contains(&name) && seen.insert(name))
        })
    })
}
fn event_names(value: &Value) -> bool {
    names(value, &EVENT_NAMES)
}
fn command_names(value: &Value) -> bool {
    names(value, &COMMAND_NAMES)
}
fn max_bytes(value: &Value) -> bool {
    int_ge1(value)
        && value
            .as_u64()
            .is_some_and(|n| n as usize <= MAX_MESSAGE_BYTES)
}
fn protocol_1(value: &Value) -> bool {
    value.as_u64() == Some(PROTOCOL)
}

/// `payloadSchemas[name]`.
pub fn event_payload_valid(name: &str, payload: &Value) -> bool {
    let notification: &[(&str, Check)] = &[("notificationId", text_1024)];
    match name {
        "notifications.shown" | "notifications.clicked" => strict(payload, notification, &[]),
        "notifications.closed" => strict(
            payload,
            &[("notificationId", text_1024), ("byUser", boolean)],
            &[],
        ),
        "tabGroups.created" | "tabGroups.updated" | "tabGroups.moved" => {
            strict(payload, &[("group", group)], &[])
        }
        "tabs.activated" => strict(
            payload,
            &[("tabId", int_ge0), ("windowId", int_ge_neg1)],
            &[("previousTabId", int_ge0)],
        ),
        "windows.focusChanged" => strict(payload, &[("windowId", int_ge_neg1)], &[]),
        _ => false,
    }
}

fn details(params: &Value, required: &[(&str, Check)], optional: &[(&str, Check)]) -> bool {
    let Some(object) = params.as_object() else {
        return false;
    };
    object.len() == 1
        && object
            .get("details")
            .is_some_and(|details| strict(details, required, optional))
}

fn title_8192(value: &Value) -> bool {
    value.as_str().is_some_and(|s| s.chars().count() <= 8192)
}
fn required_version(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        s.split_once('.').is_some_and(|(major, minor)| {
            !major.is_empty()
                && !minor.is_empty()
                && major.chars().all(|c| c.is_ascii_digit())
                && minor.chars().all(|c| c.is_ascii_digit())
        })
    })
}
fn update_properties(value: &Value) -> bool {
    strict(
        value,
        &[],
        &[
            ("collapsed", boolean),
            ("color", color),
            ("title", title_nullable),
        ],
    ) && value.as_object().is_some_and(|object| !object.is_empty())
}

/// `commandParams[name]`.
pub fn command_params_valid(name: &str, params: &Value) -> bool {
    match name {
        "bookmarks.create" => details(
            params,
            &[("title", title_8192)],
            &[
                ("parentId", nonempty),
                ("index", int_ge0),
                ("url", url_8192),
            ],
        ),
        "bookmarks.move" => details(
            params,
            &[("id", nonempty)],
            &[("parentId", nonempty), ("index", int_ge0)],
        ),
        "bookmarks.remove" => details(params, &[("id", nonempty)], &[]),
        "history.deleteUrl" => details(params, &[("url", url_8192)], &[]),
        "history.deleteRange" => {
            details(
                params,
                &[("startTime", finite_ge0), ("endTime", finite_ge0)],
                &[],
            ) && params["details"]["startTime"].as_f64() <= params["details"]["endTime"].as_f64()
        }
        "tabGroups.update" => details(
            params,
            &[
                ("groupId", int_ge0),
                ("updateProperties", update_properties),
            ],
            &[],
        ),
        "debugger.attach" => details(
            params,
            &[("tabId", int_ge0), ("requiredVersion", required_version)],
            &[],
        ),
        "debugger.detach" => details(params, &[("tabId", int_ge0)], &[]),
        _ => false,
    }
}

fn literal_true(value: &Value) -> bool {
    value == &Value::Bool(true)
}

/// `resultSchemas[name]`.
pub fn command_result_valid(name: &str, result: &Value) -> bool {
    match name {
        "bookmarks.create" | "bookmarks.move" => bookmark_node(result),
        "bookmarks.remove" | "history.deleteUrl" | "history.deleteRange" => empty(result),
        "tabGroups.update" => group(result),
        "debugger.attach" => strict(
            result,
            &[
                ("tabId", int_ge0),
                ("attached", literal_true),
                ("requiredVersion", string),
            ],
            &[],
        ),
        "debugger.detach" => strict(
            result,
            &[("tabId", int_ge0), ("detached", literal_true)],
            &[],
        ),
        _ => true,
    }
}

fn type_is(value: &Value, kind: &str) -> bool {
    value.as_str() == Some(kind)
}

/// `ExtensionHelloSchema`.
pub fn extension_hello_valid(value: &Value) -> bool {
    strict(
        value,
        &[
            ("protocol", protocol_1),
            ("type", |v| type_is(v, "hello")),
            ("role", |v| type_is(v, "extension")),
            ("connectionId", id),
            ("extensionId", extension_id),
            ("extensionVersion", version_64),
            ("events", event_names),
            ("commands", command_names),
            ("maxMessageBytes", max_bytes),
        ],
        &[],
    )
}

/// `HostHelloSchema`.
pub fn host_hello_valid(value: &Value) -> bool {
    strict(
        value,
        &[
            ("protocol", protocol_1),
            ("type", |v| type_is(v, "hello")),
            ("role", |v| type_is(v, "host")),
            ("connectionId", id),
            ("hostName", |v| type_is(v, HOST_NAME)),
            ("hostVersion", version_64),
            ("events", event_names),
            ("commands", command_names),
            ("maxMessageBytes", max_bytes),
        ],
        &[],
    )
}

/// `EventSchema`.
pub fn event_valid(value: &Value) -> bool {
    strict(
        value,
        &[
            ("protocol", protocol_1),
            ("type", |v| type_is(v, "event")),
            ("connectionId", id),
            ("seq", int_ge1),
            ("occurredAt", int_ge0),
            ("name", |v| {
                v.as_str().is_some_and(|n| EVENT_NAMES.contains(&n))
            }),
            ("payload", record),
        ],
        &[],
    ) && event_payload_valid(value["name"].as_str().unwrap_or(""), &value["payload"])
}

/// `CommandSchema`.
pub fn command_valid(value: &Value) -> bool {
    strict(
        value,
        &[
            ("protocol", protocol_1),
            ("type", |v| type_is(v, "command")),
            ("requestId", uuid_like),
            ("name", |v| {
                v.as_str().is_some_and(|n| COMMAND_NAMES.contains(&n))
            }),
            ("params", record),
        ],
        &[("deadlineAt", int_ge0)],
    ) && command_params_valid(value["name"].as_str().unwrap_or(""), &value["params"])
}

fn error_body(value: &Value) -> bool {
    strict(
        value,
        &[
            ("code", |v| {
                v.as_str().is_some_and(|c| ERROR_CODES.contains(&c))
            }),
            ("message", |v| {
                v.as_str()
                    .is_some_and(|m| (1..=512).contains(&m.chars().count()))
            }),
        ],
        &[("details", record)],
    )
}

/// `ResponseSchema`: exactly one of `result` (when `ok`) or `error`; `protocolError` is never ok.
pub fn response_valid(value: &Value) -> bool {
    let shape = strict(
        value,
        &[
            ("protocol", protocol_1),
            ("type", |v| type_is(v, "response")),
            ("requestId", uuid_like),
            ("name", |v| {
                v.as_str()
                    .is_some_and(|n| n == "protocolError" || COMMAND_NAMES.contains(&n))
            }),
            ("ok", boolean),
        ],
        &[("result", |_| true), ("error", error_body)],
    );
    if !shape {
        return false;
    }
    let name = value["name"].as_str().unwrap_or("");
    let has_result = value.get("result").is_some();
    let has_error = value.get("error").is_some();
    if value["ok"] == true {
        has_result
            && !has_error
            && name != "protocolError"
            && command_result_valid(name, &value["result"])
    } else {
        has_error && !has_result
    }
}

fn outbound_valid(value: &Value) -> Option<bool> {
    match value.get("type").and_then(Value::as_str) {
        Some("hello") => Some(if value["role"] == "host" {
            host_hello_valid(value)
        } else {
            extension_hello_valid(value)
        }),
        Some("event") => Some(event_valid(value)),
        Some("command") => Some(command_valid(value)),
        Some("response") => Some(response_valid(value)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------------------------

/// `encodeFrame`: validates known envelopes, then `u32le length + UTF-8 JSON`.
pub fn encode_frame(value: &Value, max_message_bytes: usize) -> Result<Vec<u8>, BridgeError> {
    if outbound_valid(value) == Some(false) {
        return Err(protocol("invalid outbound message"));
    }
    let body = serde_json::to_vec(value).map_err(|_| protocol("invalid JSON"))?;
    if body.is_empty() || body.len() > max_message_bytes {
        return Err(protocol("PAYLOAD_TOO_LARGE"));
    }
    let mut frame = Vec::with_capacity(body.len() + 4);
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// `decodeFrames`: every complete frame in `buffer`, and the incomplete remainder.
pub fn decode_frames(
    buffer: &[u8],
    max_message_bytes: usize,
) -> Result<(Vec<Value>, Vec<u8>), BridgeError> {
    let mut frames = Vec::new();
    let mut offset = 0;
    while buffer.len() - offset >= 4 {
        let mut length = [0u8; 4];
        length.copy_from_slice(&buffer[offset..offset + 4]);
        let length = u32::from_le_bytes(length) as usize;
        if length == 0 || length > max_message_bytes {
            return Err(protocol("PAYLOAD_TOO_LARGE"));
        }
        if buffer.len() - offset < length + 4 {
            break;
        }
        let body = &buffer[offset + 4..offset + 4 + length];
        let text = std::str::from_utf8(body).map_err(|_| protocol("invalid UTF-8"))?;
        frames.push(serde_json::from_str(text).map_err(|_| protocol("invalid JSON"))?);
        offset += length + 4;
    }
    Ok((frames, buffer[offset..].to_vec()))
}

fn random_hex() -> String {
    use std::hash::{BuildHasher, Hasher};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut out = String::new();
    for _ in 0..2 {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        hasher.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default(),
        );
        out.push_str(&format!("{:016x}", hasher.finish()));
    }
    format!(
        "{}-{}-{}-{}-{}",
        &out[0..8],
        &out[8..12],
        &out[12..16],
        &out[16..20],
        &out[20..32]
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// Host
// ---------------------------------------------------------------------------------------------

/// Receives every accepted event envelope.
pub type EventListener = Box<dyn Fn(&Value) + Send + Sync>;
/// Told once why the connection closed (`onDisconnect`).
pub type DisconnectListener = Box<dyn Fn(&BridgeError) + Send + Sync>;

/// `AbortSignal` for [`NativeMessagingHost::request_cancellable`]. Clones share one flag.
#[derive(Clone, Default)]
pub struct CancelToken {
    inner: Arc<CancelInner>,
}

#[derive(Default)]
struct CancelInner {
    cancelled: std::sync::atomic::AtomicBool,
    /// Hosts with a request waiting on this token (`addEventListener("abort")`).
    waiters: Mutex<HashMap<u64, std::sync::Weak<HostShared>>>,
    next_waiter: AtomicU64,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// `controller.abort()`: requests waiting on this token stop waiting.
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        let hosts: Vec<_> = self
            .inner
            .waiters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter_map(std::sync::Weak::upgrade)
            .collect();
        for host in hosts {
            // Taking the state lock orders this wake-up after the waiter's flag check.
            let _state = host.lock();
            host.changed.notify_all();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// Requests currently registered on this token (abort listeners not yet removed).
    pub fn waiters(&self) -> usize {
        self.inner
            .waiters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn register(&self, host: &Arc<HostShared>) -> u64 {
        let id = self.inner.next_waiter.fetch_add(1, Ordering::Relaxed);
        self.inner
            .waiters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, Arc::downgrade(host));
        id
    }

    fn unregister(&self, id: u64) {
        self.inner
            .waiters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
    }
}

/// `createNativeMessagingHost` options.
pub struct HostOptions {
    pub extension_id: String,
    pub host_name: String,
    pub max_message_bytes: usize,
    pub command_timeout: Duration,
    pub max_pending: usize,
    pub on_event: Option<EventListener>,
    pub on_disconnect: Option<DisconnectListener>,
}

impl HostOptions {
    pub fn new(extension_id: &str) -> Self {
        Self {
            extension_id: extension_id.to_owned(),
            host_name: HOST_NAME.to_owned(),
            max_message_bytes: MAX_MESSAGE_BYTES,
            command_timeout: Duration::from_secs(30),
            max_pending: 256,
            on_event: None,
            on_disconnect: None,
        }
    }
}

#[derive(Default)]
struct HostState {
    hello: Option<Value>,
    host_connection_id: Option<String>,
    effective_max: usize,
    last_event_seq: u64,
    sequence: u64,
    closed: Option<BridgeError>,
    /// Requests admitted (queued or in flight).
    admitted: usize,
    /// The request id of the mutation in flight.
    in_flight: Option<String>,
    responses: HashMap<String, Result<Value, BridgeError>>,
    /// Admitted mutations in call order; the front one may run.
    queue: std::collections::VecDeque<u64>,
    next_ticket: u64,
}

struct HostShared {
    state: Mutex<HostState>,
    changed: Condvar,
    writer: Mutex<Box<dyn Write + Send>>,
    errors: Mutex<Box<dyn Write + Send>>,
    listeners: Mutex<HashMap<String, Vec<(u64, Arc<EventListener>)>>>,
    next_listener: AtomicU64,
    options: HostOptions,
}

impl HostShared {
    fn lock(&self) -> MutexGuard<'_, HostState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn report(&self, message: &str) {
        let mut errors = self.errors.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = writeln!(errors, "{message}");
    }

    fn disconnect(&self, error: BridgeError) {
        let mut state = self.lock();
        let first = state.closed.is_none();
        if first {
            state.closed = Some(error.clone());
        }
        self.changed.notify_all();
        drop(state);
        if first && let Some(on_disconnect) = &self.options.on_disconnect {
            on_disconnect(&error);
        }
    }

    fn write(&self, value: &Value) -> Result<(), BridgeError> {
        if let Some(error) = &self.lock().closed {
            return Err(error.clone());
        }
        if outbound_valid(value).is_none() {
            return Err(protocol("invalid outbound message type"));
        }
        let max = self.lock().effective_max;
        let frame = encode_frame(value, max)?;
        let mut writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let written = writer.write_all(&frame).and_then(|()| writer.flush());
        drop(writer);
        written.map_err(|error| {
            let error = BridgeError::Unavailable(format!("native host write failed: {error}"));
            self.disconnect(error.clone());
            error
        })
    }

    fn dispatch_event(&self, event: Value) -> Result<(), BridgeError> {
        {
            let mut state = self.lock();
            let hello = state.hello.clone().unwrap_or_default();
            let name = event["name"].as_str().unwrap_or("");
            let seq = event["seq"].as_u64().unwrap_or(0);
            let negotiated = hello["events"]
                .as_array()
                .is_some_and(|events| events.iter().any(|known| known == name));
            if event["connectionId"] != hello["connectionId"]
                || seq <= state.last_event_seq
                || !negotiated
            {
                return Err(protocol("invalid event ordering or capability"));
            }
            state.last_event_seq = seq;
        }
        if let Some(on_event) = &self.options.on_event {
            on_event(&event);
        }
        let listeners: Vec<Arc<EventListener>> = self
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(event["name"].as_str().unwrap_or(""))
            .map(|listeners| listeners.iter().map(|(_, l)| Arc::clone(l)).collect())
            .unwrap_or_default();
        for listener in listeners {
            (**listener)(&event["payload"]);
        }
        Ok(())
    }

    fn handle(&self, raw: Value) -> Result<(), BridgeError> {
        if self.lock().hello.is_none() {
            if !extension_hello_valid(&raw)
                || raw["extensionId"] != self.options.extension_id.as_str()
            {
                return Err(protocol("extension hello rejected"));
            }
            let connection = random_hex();
            let max = self
                .options
                .max_message_bytes
                .min(raw["maxMessageBytes"].as_u64().unwrap_or(0) as usize);
            let negotiate = |all: &[&str], key: &str| -> Vec<String> {
                all.iter()
                    .filter(|name| {
                        raw[key]
                            .as_array()
                            .is_some_and(|list| list.iter().any(|v| v.as_str() == Some(**name)))
                    })
                    .map(|name| (*name).to_owned())
                    .collect()
            };
            let events = negotiate(&EVENT_NAMES, "events");
            let commands = negotiate(&COMMAND_NAMES, "commands");
            {
                let mut state = self.lock();
                state.hello = Some(raw.clone());
                state.host_connection_id = Some(connection.clone());
                state.effective_max = max;
                state.last_event_seq = 0;
                self.changed.notify_all();
            }
            return self.write(&json!({
                "protocol": PROTOCOL, "type": "hello", "role": "host", "connectionId": connection,
                "hostName": self.options.host_name, "hostVersion": "0.1.0", "events": events,
                "commands": commands, "maxMessageBytes": max,
            }));
        }
        match raw.get("type").and_then(Value::as_str) {
            Some("event") => {
                if !event_valid(&raw) {
                    return Err(protocol("invalid event"));
                }
                self.dispatch_event(raw)
            }
            Some("response") => {
                if !response_valid(&raw) {
                    return Err(protocol("invalid response"));
                }
                if raw["name"] == "protocolError" {
                    return Err(protocol(
                        raw["error"]["message"].as_str().unwrap_or("protocol error"),
                    ));
                }
                let request_id = raw["requestId"].as_str().unwrap_or("").to_owned();
                let mut state = self.lock();
                if state.in_flight.as_deref() != Some(request_id.as_str()) {
                    return Err(protocol("invalid or unknown response"));
                }
                let outcome = if raw["ok"] == true {
                    Ok(raw["result"].clone())
                } else {
                    Err(BridgeError::Command {
                        code: raw["error"]["code"].as_str().map(str::to_owned),
                        message: raw["error"]["message"].as_str().unwrap_or("").to_owned(),
                    })
                };
                state.responses.insert(request_id, outcome);
                state.in_flight = None;
                self.changed.notify_all();
                Ok(())
            }
            _ => Err(protocol("unexpected message")),
        }
    }
}

/// The native messaging host side of the bridge.
#[derive(Clone)]
pub struct NativeMessagingHost {
    shared: Arc<HostShared>,
}

impl NativeMessagingHost {
    /// Starts reading framed messages from `stdin`; writes go to `stdout`, diagnostics to
    /// `stderr`.
    pub fn new(
        stdin: Box<dyn Read + Send>,
        stdout: Box<dyn Write + Send>,
        stderr: Box<dyn Write + Send>,
        options: HostOptions,
    ) -> Result<Self, BridgeError> {
        if !extension_id(&json!(options.extension_id)) {
            return Err(protocol("a valid extensionId is required"));
        }
        if options.host_name != HOST_NAME || !valid_host_name(&options.host_name) {
            return Err(protocol("invalid hostName"));
        }
        let shared = Arc::new(HostShared {
            state: Mutex::new(HostState {
                effective_max: options.max_message_bytes,
                ..HostState::default()
            }),
            changed: Condvar::new(),
            writer: Mutex::new(stdout),
            errors: Mutex::new(stderr),
            listeners: Mutex::new(HashMap::new()),
            next_listener: AtomicU64::new(0),
            options,
        });
        let reader = Arc::clone(&shared);
        let mut stdin = stdin;
        std::thread::Builder::new()
            .name("bridge-host-reader".into())
            .spawn(move || {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    if reader.lock().closed.is_some() {
                        return;
                    }
                    let read = match stdin.read(&mut chunk) {
                        Ok(0) | Err(_) => {
                            reader.disconnect(BridgeError::Unavailable(
                                "native host disconnected".into(),
                            ));
                            return;
                        }
                        Ok(read) => read,
                    };
                    buffer.extend_from_slice(&chunk[..read]);
                    let max = reader.lock().effective_max;
                    let (frames, rest) = match decode_frames(&buffer, max) {
                        Ok(decoded) => decoded,
                        Err(error) => {
                            reader.report(&format!("bridge: {error}"));
                            reader.disconnect(error);
                            return;
                        }
                    };
                    buffer = rest;
                    for raw in frames {
                        if let Err(error) = reader.handle(raw) {
                            reader.report(&format!("bridge: {error}"));
                            reader.disconnect(error);
                            return;
                        }
                    }
                }
            })
            .map(drop)
            .map_err(|error| {
                BridgeError::Unavailable(format!("cannot start the reader: {error}"))
            })?;
        Ok(Self { shared })
    }

    /// Whether the extension said hello and the connection is open.
    pub fn is_connected(&self) -> bool {
        let state = self.shared.lock();
        state.closed.is_none() && state.hello.is_some()
    }

    /// Negotiated command and event names.
    pub fn capabilities(&self) -> (HashSet<String>, HashSet<String>) {
        let state = self.shared.lock();
        let list = |key: &str| {
            state
                .hello
                .as_ref()
                .and_then(|hello| hello[key].as_array())
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<HashSet<_>>()
        };
        (list("commands"), list("events"))
    }

    /// Waits until the extension's hello arrived (or the connection closed).
    pub fn wait_connected(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.lock();
        while state.hello.is_none() && state.closed.is_none() {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        state.closed.is_none()
    }

    /// Waits until the connection closed; `false` on timeout.
    pub fn wait_disconnected(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.lock();
        while state.closed.is_none() {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    /// `send`: writes a validated envelope.
    pub fn send(&self, value: &Value) -> Result<(), BridgeError> {
        self.shared.write(value)
    }

    /// `request`: sends a mutation and waits for its response. Mutations run one at a time in
    /// call order; a timeout fails the connection, since the extension may have applied it.
    pub fn request(
        &self,
        name: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, BridgeError> {
        self.request_inner(name, params, timeout, None)
    }

    /// [`Self::request`] with `{ signal }`: cancelling `cancel` while the mutation is queued
    /// rejects it with `ABORT_ERR`; once it was sent, the rejection also disconnects (completion
    /// is indeterminate).
    pub fn request_cancellable(
        &self,
        name: &str,
        params: Value,
        timeout: Option<Duration>,
        cancel: &CancelToken,
    ) -> Result<Value, BridgeError> {
        self.request_inner(name, params, timeout, Some(cancel))
    }

    fn request_inner(
        &self,
        name: &str,
        params: Value,
        timeout: Option<Duration>,
        cancel: Option<&CancelToken>,
    ) -> Result<Value, BridgeError> {
        let timeout = timeout.unwrap_or(self.shared.options.command_timeout);
        let request_id = random_hex();
        let command = json!({"protocol": PROTOCOL, "type": "command", "requestId": request_id, "name": name, "params": params});
        let ticket;
        {
            let mut state = self.shared.lock();
            if let Some(error) = &state.closed {
                return Err(error.clone());
            }
            let negotiated = state.hello.as_ref().is_some_and(|hello| {
                hello["commands"]
                    .as_array()
                    .is_some_and(|commands| commands.iter().any(|known| known == name))
            });
            if !negotiated {
                return Err(BridgeError::Unavailable(format!(
                    "unsupported bridge command: {name}"
                )));
            }
            if state.admitted >= self.shared.options.max_pending {
                return Err(protocol("BOUNDED_TRANSPORT"));
            }
            if !command_valid(&command) {
                return Err(protocol("invalid command parameters"));
            }
            if cancel.is_some_and(CancelToken::is_cancelled) {
                return Err(aborted(name));
            }
            state.admitted += 1;
            ticket = state.next_ticket;
            state.next_ticket += 1;
            state.queue.push_back(ticket);
            self.shared.changed.notify_all();
        }
        let registration = cancel.map(|token| (token, token.register(&self.shared)));
        let outcome = self.run(ticket, name, &request_id, &command, timeout, cancel);
        if let Some((token, id)) = registration {
            token.unregister(id);
        }
        let mut state = self.shared.lock();
        state.admitted -= 1;
        state.queue.retain(|queued| *queued != ticket);
        self.shared.changed.notify_all();
        outcome
    }

    fn run(
        &self,
        ticket: u64,
        name: &str,
        request_id: &str,
        command: &Value,
        timeout: Duration,
        cancel: Option<&CancelToken>,
    ) -> Result<Value, BridgeError> {
        let deadline = Instant::now() + timeout;
        let mut started = false;
        let mut state = self.shared.lock();
        loop {
            if let Some(outcome) = state.responses.remove(request_id) {
                return outcome;
            }
            if let Some(error) = &state.closed {
                return Err(error.clone());
            }
            if cancel.is_some_and(CancelToken::is_cancelled) {
                // `failIndeterminate("ABORT_ERR")`.
                if started {
                    state.in_flight = None;
                    drop(state);
                    self.shared.disconnect(BridgeError::Command {
                        code: Some("ABORT_ERR".into()),
                        message: "mutation completion is indeterminate".into(),
                    });
                }
                return Err(aborted(name));
            }
            if !started && state.queue.front() == Some(&ticket) && state.in_flight.is_none() {
                state.in_flight = Some(request_id.to_owned());
                started = true;
                drop(state);
                if let Err(error) = self.shared.write(command) {
                    self.shared.disconnect(error.clone());
                    return Err(error);
                }
                state = self.shared.lock();
                continue;
            }
            let now = Instant::now();
            if now >= deadline {
                let error = BridgeError::Command {
                    code: Some("TIMEOUT".into()),
                    message: format!("bridge command timed out: {name}"),
                };
                if started {
                    drop(state);
                    self.shared.disconnect(BridgeError::Command {
                        code: Some("TIMEOUT".into()),
                        message: "mutation completion is indeterminate".into(),
                    });
                }
                return Err(error);
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Subscribes to one event name; returns an id for [`Self::off`].
    pub fn on(&self, name: &str, listener: EventListener) -> Result<u64, BridgeError> {
        if !EVENT_NAMES.contains(&name) {
            return Err(protocol("invalid event name"));
        }
        let id = self.shared.next_listener.fetch_add(1, Ordering::Relaxed);
        self.shared
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(name.to_owned())
            .or_default()
            .push((id, Arc::new(listener)));
        Ok(id)
    }

    pub fn off(&self, id: u64) {
        let mut listeners = self
            .shared
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for list in listeners.values_mut() {
            list.retain(|(known, _)| *known != id);
        }
        listeners.retain(|_, list| !list.is_empty());
    }

    /// `emitEvent`: sends a host event with the next sequence number.
    pub fn emit_event(&self, name: &str, payload: Value) -> Result<(), BridgeError> {
        let (connection, seq) = {
            let mut state = self.shared.lock();
            let negotiated = state.hello.as_ref().is_some_and(|hello| {
                hello["events"]
                    .as_array()
                    .is_some_and(|events| events.iter().any(|known| known == name))
            });
            if !negotiated {
                return Err(BridgeError::Unavailable("unsupported bridge event".into()));
            }
            if !event_payload_valid(name, &payload) {
                return Err(protocol("invalid event payload"));
            }
            state.sequence += 1;
            (state.host_connection_id.clone(), state.sequence)
        };
        self.shared.write(&json!({
            "protocol": PROTOCOL, "type": "event", "connectionId": connection, "seq": seq,
            "occurredAt": now_ms(), "name": name, "payload": payload,
        }))
    }

    /// Closes the connection; pending and later requests fail.
    pub fn close(&self) {
        self.shared
            .disconnect(BridgeError::Unavailable("closed".into()));
    }
}

fn valid_host_name(name: &str) -> bool {
    !name.is_empty()
        && name.split(['.', '-']).all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

// ---------------------------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------------------------

/// `defaultNativeMessagingHostDir(browser, platform)` (`platform` as Node names it).
pub fn default_native_messaging_host_dir(
    browser: &str,
    platform: &str,
    home: &Path,
) -> Result<PathBuf, BridgeError> {
    let segments: &[&str] = match (platform, browser) {
        ("darwin", "chrome") => &["Library", "Application Support", "Google", "Chrome"],
        ("darwin", "chromium") => &["Library", "Application Support", "Chromium"],
        ("linux", "chrome") => &[".config", "google-chrome"],
        ("linux", "chromium") => &[".config", "chromium"],
        ("darwin" | "linux", _) => {
            return Err(protocol("custom browser requires nativeMessagingHostDir"));
        }
        _ => {
            return Err(protocol(&format!(
                "no default nativeMessagingHostDir on {platform}; pass nativeMessagingHostDir"
            )));
        }
    };
    let mut dir = home.to_path_buf();
    dir.extend(segments);
    Ok(dir.join("NativeMessagingHosts"))
}

/// `installNativeMessagingHost` options.
#[derive(Clone, Debug)]
pub struct InstallOptions {
    pub host_name: String,
    pub host_path: PathBuf,
    pub extension_id: String,
    pub browser: String,
    pub native_messaging_host_dir: Option<PathBuf>,
    pub force: bool,
    /// Always rejected (kept to reject callers that still pass it).
    pub endpoint_path: Option<PathBuf>,
}

impl InstallOptions {
    pub fn new(host_path: impl Into<PathBuf>, extension_id: &str) -> Self {
        Self {
            host_name: HOST_NAME.to_owned(),
            host_path: host_path.into(),
            extension_id: extension_id.to_owned(),
            browser: "chrome".into(),
            native_messaging_host_dir: None,
            force: false,
            endpoint_path: None,
        }
    }
}

#[cfg(unix)]
fn current_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    // std has no getuid: a file this process creates is owned by its effective uid.
    let probe = std::env::temp_dir().join(format!(".maho-uid-{}", random_hex()));
    std::fs::File::create(&probe).ok()?;
    let uid = std::fs::metadata(&probe).ok().map(|meta| meta.uid());
    let _ = std::fs::remove_file(&probe);
    uid
}

#[cfg(unix)]
fn secure_directory(dir: &Path, uid: u32) -> Result<(), BridgeError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let absolute = std::path::absolute(dir).map_err(|e| protocol(&e.to_string()))?;
    let parts: Vec<_> = absolute
        .components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .collect();
    let mut current = PathBuf::from("/");
    let mut last = None;
    for (index, part) in parts.iter().enumerate() {
        current.push(part);
        let mut info = std::fs::symlink_metadata(&current).ok();
        if info.is_none() {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&current)
                .map_err(|e| protocol(&format!("cannot create {}: {e}", current.display())))?;
            info = std::fs::symlink_metadata(&current).ok();
        }
        let info = info.ok_or_else(|| protocol("nativeMessagingHostDir security rejected"))?;
        let is_var = current == Path::new("/var");
        if !is_var && (info.file_type().is_symlink() || !info.is_dir()) {
            return Err(protocol("nativeMessagingHostDir security rejected"));
        }
        if !is_var && info.uid() != uid && info.uid() != 0 {
            return Err(protocol("nativeMessagingHostDir ownership rejected"));
        }
        let sticky_root_ancestor =
            index + 1 < parts.len() && info.uid() == 0 && info.mode() & 0o1000 != 0;
        if !is_var && !sticky_root_ancestor && info.mode() & 0o022 != 0 {
            return Err(protocol("nativeMessagingHostDir security rejected"));
        }
        last = Some(info);
    }
    let last = last.ok_or_else(|| protocol("nativeMessagingHostDir ownership rejected"))?;
    if last.uid() != uid && last.uid() != 0 {
        return Err(protocol("nativeMessagingHostDir ownership rejected"));
    }
    if last.mode() & 0o077 != 0 {
        std::fs::set_permissions(&absolute, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| protocol(&e.to_string()))?;
    }
    Ok(())
}

#[cfg(unix)]
fn secure_executable(path: &Path, uid: u32) -> Result<(), BridgeError> {
    use std::os::unix::fs::MetadataExt;
    let ok = std::fs::symlink_metadata(path).is_ok_and(|info| {
        info.is_file()
            && !info.file_type().is_symlink()
            && info.uid() == uid
            && info.mode() & 0o111 != 0
            && info.mode() & 0o022 == 0
    });
    if ok {
        Ok(())
    } else {
        Err(protocol(
            "hostPath must be a securely owned executable file",
        ))
    }
}

/// `installNativeMessagingHost`: writes the pinned manifest atomically with mode `0600`.
/// Returns the manifest path and its content.
#[cfg(unix)]
pub fn install_native_messaging_host(
    options: &InstallOptions,
    home: &Path,
) -> Result<(PathBuf, Value), BridgeError> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if options.endpoint_path.is_some() {
        return Err(protocol("endpointPath is not supported"));
    }
    if options.host_name != HOST_NAME || !valid_host_name(&options.host_name) {
        return Err(protocol("invalid hostName"));
    }
    if !options.host_path.is_absolute() {
        return Err(protocol("hostPath must be absolute"));
    }
    let uid = current_uid().ok_or_else(|| protocol("cannot determine the current user"))?;
    secure_executable(&options.host_path, uid)?;
    if !extension_id(&json!(options.extension_id)) {
        return Err(protocol("invalid extensionId"));
    }
    if options.native_messaging_host_dir.is_none() && options.browser == "cloakbrowser" {
        return Err(protocol(
            "CloakBrowser nativeMessagingHostDir must be explicit and verified",
        ));
    }
    let platform = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let dir = match &options.native_messaging_host_dir {
        Some(dir) => dir.clone(),
        None => default_native_messaging_host_dir(&options.browser, platform, home)?,
    };
    secure_directory(&dir, uid)?;
    let manifest = json!({
        "name": options.host_name,
        "description": "OmOWright internal CloakBrowser native messaging bridge.",
        "path": options.host_path.display().to_string(),
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{}/", options.extension_id)],
    });
    let file = dir.join(format!("{}.json", options.host_name));
    if !options.force && std::fs::symlink_metadata(&file).is_ok() {
        return Err(protocol("registration manifest already exists; use force"));
    }
    let temp = dir.join(format!(
        "{}.json.{}.{}.tmp",
        options.host_name,
        std::process::id(),
        random_hex()
    ));
    let io = |e: std::io::Error| protocol(&e.to_string());
    {
        let mut handle = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(io)?;
        let text = format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).unwrap_or_default()
        );
        handle.write_all(text.as_bytes()).map_err(io)?;
        handle.sync_all().map_err(io)?;
    }
    std::fs::rename(&temp, &file).map_err(io)?;
    let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&file).map_err(io)?)
        .map_err(|_| protocol("invalid JSON"))?;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).map_err(io)?;
    Ok((file, parsed))
}

// ---------------------------------------------------------------------------------------------
// createChromeApi (bridge-routed part)
// ---------------------------------------------------------------------------------------------

/// The `chrome.*` wire calls routed through the bridge (`createChromeApi`): each needs the
/// command negotiated, else the upstream "requires the extension bridge" error.
pub struct ChromeBridgeApi<'a> {
    pub bridge: Option<&'a NativeMessagingHost>,
}

impl ChromeBridgeApi<'_> {
    fn request(&self, wire: &str, params: Value) -> Result<Value, BridgeError> {
        match self.bridge {
            Some(bridge) if bridge.is_connected() && bridge.capabilities().0.contains(wire) => {
                bridge.request(wire, params, None)
            }
            _ => Err(BridgeError::Unavailable(format!(
                "chrome.{wire} requires the extension bridge in standalone OmOWright"
            ))),
        }
    }

    pub fn bookmarks_create(&self, details: Value) -> Result<Value, BridgeError> {
        self.request("bookmarks.create", json!({"details": details}))
    }

    pub fn bookmarks_move(&self, id: &str, details: Value) -> Result<Value, BridgeError> {
        let mut details = details;
        if !details.is_object() {
            details = json!({});
        }
        details["id"] = json!(id);
        self.request("bookmarks.move", json!({"details": details}))
    }

    pub fn bookmarks_remove(&self, id: &str) -> Result<Value, BridgeError> {
        self.request("bookmarks.remove", json!({"details": {"id": id}}))
    }

    pub fn history_delete_url(&self, details: Value) -> Result<Value, BridgeError> {
        self.request("history.deleteUrl", json!({"details": details}))
    }

    pub fn history_delete_range(&self, details: Value) -> Result<Value, BridgeError> {
        self.request("history.deleteRange", json!({"details": details}))
    }

    pub fn tab_groups_update(
        &self,
        group_id: u64,
        update_properties: Value,
    ) -> Result<Value, BridgeError> {
        self.request(
            "tabGroups.update",
            json!({"details": {"groupId": group_id, "updateProperties": update_properties}}),
        )
    }

    pub fn debugger_attach(
        &self,
        tab_id: u64,
        required_version: &str,
    ) -> Result<Value, BridgeError> {
        self.request(
            "debugger.attach",
            json!({"details": {"tabId": tab_id, "requiredVersion": required_version}}),
        )
    }

    pub fn debugger_detach(&self, tab_id: u64) -> Result<Value, BridgeError> {
        self.request("debugger.detach", json!({"details": {"tabId": tab_id}}))
    }
}

/// The wire event name of a `chrome.<namespace>.<onEvent>` listener (`onShown` -> `shown`).
pub fn chrome_event_wire(namespace: &str, event: &str) -> Option<String> {
    let rest = event.strip_prefix("on")?;
    let mut chars = rest.chars();
    let first = chars.next()?.to_ascii_lowercase();
    let wire = format!("{namespace}.{first}{}", chars.as_str());
    EVENT_NAMES.contains(&wire.as_str()).then_some(wire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const EXTENSION_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn hello() -> Value {
        json!({"protocol": 1, "type": "hello", "role": "extension", "connectionId": "c1",
               "extensionId": EXTENSION_ID, "extensionVersion": "0.1.0", "events": ["tabs.activated"],
               "commands": ["bookmarks.create"], "maxMessageBytes": 1024 * 1024})
    }

    /// Bytes written to `tx` are read by the host; the host's output frames arrive on `rx`.
    struct Pipe {
        input: mpsc::Sender<Vec<u8>>,
        output: mpsc::Receiver<Vec<u8>>,
        errors: Arc<Mutex<Vec<u8>>>,
        pending: Vec<u8>,
    }

    struct ChannelReader {
        rx: mpsc::Receiver<Vec<u8>>,
        rest: Vec<u8>,
    }

    impl Read for ChannelReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.rest.is_empty() {
                match self.rx.recv() {
                    Ok(bytes) => self.rest = bytes,
                    Err(_) => return Ok(0),
                }
            }
            let n = buf.len().min(self.rest.len());
            buf[..n].copy_from_slice(&self.rest[..n]);
            self.rest.drain(..n);
            Ok(n)
        }
    }

    struct ChannelWriter(mpsc::Sender<Vec<u8>>);

    impl Write for ChannelWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .send(buf.to_vec())
                .map_err(|_| std::io::Error::other("closed"))?;
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn host(options: HostOptions) -> (NativeMessagingHost, Pipe) {
        let (in_tx, in_rx) = mpsc::channel();
        let (out_tx, out_rx) = mpsc::channel();
        let errors = Arc::new(Mutex::new(Vec::new()));
        let host = NativeMessagingHost::new(
            Box::new(ChannelReader {
                rx: in_rx,
                rest: Vec::new(),
            }),
            Box::new(ChannelWriter(out_tx)),
            Box::new(SharedWriter(Arc::clone(&errors))),
            options,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        (
            host,
            Pipe {
                input: in_tx,
                output: out_rx,
                errors,
                pending: Vec::new(),
            },
        )
    }

    impl Pipe {
        fn write(&self, value: &Value) {
            let frame = encode_frame(value, MAX_MESSAGE_BYTES).unwrap_or_else(|e| panic!("{e}"));
            let _ = self.input.send(frame);
        }

        fn write_raw(&self, value: &Value) {
            let body = serde_json::to_vec(value).unwrap_or_default();
            let mut frame = (body.len() as u32).to_le_bytes().to_vec();
            frame.extend(body);
            let _ = self.input.send(frame);
        }

        fn read_one(&mut self) -> Value {
            loop {
                if self.pending.len() >= 4 {
                    let length = u32::from_le_bytes([
                        self.pending[0],
                        self.pending[1],
                        self.pending[2],
                        self.pending[3],
                    ]) as usize;
                    if self.pending.len() >= 4 + length {
                        let frame: Vec<u8> = self.pending.drain(..4 + length).collect();
                        let (frames, _) = decode_frames(&frame, MAX_MESSAGE_BYTES)
                            .unwrap_or_else(|e| panic!("{e}"));
                        return frames.into_iter().next().unwrap_or_default();
                    }
                }
                let chunk = self
                    .output
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap_or_else(|_| panic!("no frame from the host"));
                self.pending.extend(chunk);
            }
        }

        fn nothing_sent(&self) -> bool {
            self.pending.is_empty() && self.output.try_recv().is_err()
        }
    }

    fn connect(options: HostOptions) -> (NativeMessagingHost, Pipe) {
        let (host, mut pipe) = host(options);
        pipe.write(&hello());
        let host_hello = pipe.read_one();
        assert!(host.wait_connected(Duration::from_secs(5)));
        assert!(host_hello_valid(&host_hello), "{host_hello}");
        (host, pipe)
    }

    fn respond(pipe: &Pipe, sent: &Value, result: Value) {
        pipe.write(&json!({"protocol": 1, "type": "response", "requestId": sent["requestId"], "name": sent["name"], "ok": true, "result": result}));
    }

    #[test]
    fn native_frames_support_splits_and_multiple_messages_with_exact_binary_contract() {
        let frame =
            encode_frame(&json!({"ok": true}), MAX_MESSAGE_BYTES).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize,
            frame.len() - 4
        );
        assert_eq!(frame.last(), Some(&0x7d));
        assert!(!frame.contains(&0x0a));
        for split in 0..=frame.len() {
            let (first, rest) =
                decode_frames(&frame[..split], MAX_MESSAGE_BYTES).unwrap_or_else(|e| panic!("{e}"));
            let mut joined = rest;
            joined.extend_from_slice(&frame[split..]);
            let (second, _) =
                decode_frames(&joined, MAX_MESSAGE_BYTES).unwrap_or_else(|e| panic!("{e}"));
            let all: Vec<Value> = first.into_iter().chain(second).collect();
            assert_eq!(all, [json!({"ok": true})]);
        }
        let double = [frame.clone(), frame].concat();
        assert_eq!(
            decode_frames(&double, MAX_MESSAGE_BYTES).map(|(frames, _)| frames),
            Ok(vec![json!({"ok": true}), json!({"ok": true})])
        );
    }

    #[test]
    fn native_frame_parser_rejects_malformed_and_oversized_messages() {
        assert!(decode_frames(&[0, 0, 0, 0], MAX_MESSAGE_BYTES).is_err());
        assert!(decode_frames(&[1, 0, 0, 0, 0xff], MAX_MESSAGE_BYTES).is_err());
        assert!(encode_frame(&json!({"x": "x"}), 1).is_err());
    }

    #[test]
    fn strict_schemas_reject_unknown_names_and_fields() {
        assert!(extension_hello_valid(&hello()));
        let mut extra = hello();
        extra["extra"] = json!(true);
        assert!(!extension_hello_valid(&extra));
        assert!(!event_valid(
            &json!({"protocol": 1, "type": "event", "connectionId": "c1", "seq": 1, "occurredAt": 1, "name": "tabs.activated", "payload": {"tabId": 2, "windowId": 1, "extra": true}})
        ));
    }

    #[test]
    fn host_validates_hello_negotiates_capabilities_correlates_response_and_forwards_events() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&events);
        let mut options = HostOptions::new(EXTENSION_ID);
        options.on_event = Some(Box::new(move |event: &Value| {
            seen.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event.clone());
        }));
        let (host, mut pipe) = self::host(options);
        pipe.write(&hello());
        let host_hello = pipe.read_one();
        assert_eq!(host_hello["protocol"], 1);
        assert_eq!(host_hello["role"], "host");
        assert!(host_hello["connectionId"].is_string());
        assert_eq!(host_hello["hostName"], "com.omowright.cloakbridge");
        assert_eq!(host_hello["commands"], json!(["bookmarks.create"]));
        let worker = std::thread::spawn({
            let host = host.clone();
            move || host.request("bookmarks.create", json!({"details": {"title": "x"}}), None)
        });
        let sent = pipe.read_one();
        assert_eq!(sent["name"], "bookmarks.create");
        respond(&pipe, &sent, json!({"id": "1", "title": "x"}));
        assert_eq!(
            worker.join().ok().and_then(Result::ok),
            Some(json!({"id": "1", "title": "x"}))
        );
        let (tx, rx) = mpsc::channel();
        let _ = host.on(
            "tabs.activated",
            Box::new(move |payload: &Value| {
                let _ = tx.send(payload.clone());
            }),
        );
        pipe.write(&json!({"protocol": 1, "type": "event", "connectionId": "c1", "seq": 1, "occurredAt": 1, "name": "tabs.activated", "payload": {"tabId": 4, "windowId": 1}}));
        let payload = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
        assert_eq!(payload["tabId"], 4);
        assert_eq!(
            events.lock().unwrap_or_else(PoisonError::into_inner)[0]["payload"]["tabId"],
            4
        );
        host.close();
    }

    #[test]
    fn decoder_accepts_literal_replacement_characters_but_rejects_malformed_utf8() {
        let frame = encode_frame(&json!({"text": "literal \\ufffd"}), MAX_MESSAGE_BYTES)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            decode_frames(&frame, MAX_MESSAGE_BYTES).map(|(f, _)| f),
            Ok(vec![json!({"text": "literal \\ufffd"})])
        );
        assert_eq!(
            decode_frames(&[1, 0, 0, 0, 0xc3], MAX_MESSAGE_BYTES).err(),
            Some(protocol("invalid UTF-8"))
        );
    }

    #[test]
    fn mutations_wait_for_the_prior_response_and_duplicate_event_sequences_disconnect() {
        let (host, mut pipe) = connect(HostOptions::new(EXTENSION_ID));
        let first = std::thread::spawn({
            let host = host.clone();
            move || {
                host.request(
                    "bookmarks.create",
                    json!({"details": {"title": "one"}}),
                    None,
                )
            }
        });
        let sent1 = pipe.read_one();
        let second = std::thread::spawn({
            let host = host.clone();
            move || {
                host.request(
                    "bookmarks.create",
                    json!({"details": {"title": "two"}}),
                    None,
                )
            }
        });
        // The second request is admitted but must not be written before the first response.
        {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut state = host.shared.lock();
            while state.admitted < 2 && Instant::now() < deadline {
                let left = deadline.saturating_duration_since(Instant::now());
                state = host
                    .shared
                    .changed
                    .wait_timeout(state, left)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            assert_eq!(state.admitted, 2);
        }
        assert!(pipe.nothing_sent());
        respond(&pipe, &sent1, json!({"id": "1", "title": "one"}));
        assert!(first.join().ok().is_some_and(|r| r.is_ok()));
        let sent2 = pipe.read_one();
        assert_eq!(sent2["params"]["details"]["title"], "two");
        respond(&pipe, &sent2, json!({"id": "2", "title": "two"}));
        assert!(second.join().ok().is_some_and(|r| r.is_ok()));
        let event = json!({"protocol": 1, "type": "event", "connectionId": "c1", "seq": 1, "occurredAt": 1, "name": "tabs.activated", "payload": {"tabId": 1, "windowId": 1}});
        pipe.write(&event);
        pipe.write(&event);
        assert!(host.wait_disconnected(Duration::from_secs(5)));
        assert!(!host.is_connected());
    }

    #[test]
    fn timed_out_mutation_fails_the_connection_without_sending_its_successor() {
        let mut options = HostOptions::new(EXTENSION_ID);
        options.command_timeout = Duration::from_millis(10);
        let (host, mut pipe) = connect(options);
        let first = std::thread::spawn({
            let host = host.clone();
            move || {
                host.request(
                    "bookmarks.create",
                    json!({"details": {"title": "one"}}),
                    None,
                )
            }
        });
        let _ = pipe.read_one();
        let outcome = first.join().ok().and_then(Result::err);
        assert_eq!(
            outcome.as_ref().and_then(BridgeError::code),
            Some("TIMEOUT")
        );
        let second = host.request(
            "bookmarks.create",
            json!({"details": {"title": "two"}}),
            None,
        );
        assert!(
            second
                .err()
                .is_some_and(|error| error.message().contains("indeterminate"))
        );
        assert!(!host.is_connected());
        assert!(pipe.nothing_sent());
    }

    fn create(
        host: &NativeMessagingHost,
        title: &str,
        cancel: &CancelToken,
    ) -> std::thread::JoinHandle<Result<Value, BridgeError>> {
        let host = host.clone();
        let cancel = cancel.clone();
        let title = title.to_owned();
        std::thread::spawn(move || {
            host.request_cancellable(
                "bookmarks.create",
                json!({"details": {"title": title}}),
                None,
                &cancel,
            )
        })
    }

    #[test]
    fn abort_after_a_received_mutation_response_does_not_disconnect_the_bridge() {
        let (host, mut pipe) = connect(HostOptions::new(EXTENSION_ID));
        let cancel = CancelToken::new();
        let request = create(&host, "done", &cancel);
        let sent = pipe.read_one();
        respond(&pipe, &sent, json!({"id": "1", "title": "done"}));
        let result = request.join().ok().and_then(Result::ok);
        assert_eq!(result, Some(json!({"id": "1", "title": "done"})));
        cancel.cancel();
        assert!(host.is_connected());
        host.close();
    }

    #[test]
    fn settled_mutations_remove_abort_listeners_and_timers() {
        let mut options = HostOptions::new(EXTENSION_ID);
        options.command_timeout = Duration::from_secs(60);
        let (host, mut pipe) = connect(options);
        let cancel = CancelToken::new();
        let request = create(&host, "done", &cancel);
        let sent = pipe.read_one();
        // The request is past admission once its command was written.
        assert_eq!(cancel.waiters(), 1);
        respond(&pipe, &sent, json!({"id": "1", "title": "done"}));
        assert!(request.join().ok().is_some_and(|result| result.is_ok()));
        assert_eq!(cancel.waiters(), 0);
        cancel.cancel();
        assert!(host.is_connected());
        host.close();
    }

    #[test]
    fn write_rejection_removes_the_abort_listener() {
        let mut options = HostOptions::new(EXTENSION_ID);
        options.max_pending = 1;
        options.command_timeout = Duration::from_secs(60);
        let (host, mut pipe) = connect(options);
        let first = create(&host, "one", &CancelToken::new());
        let sent = pipe.read_one();
        let cancel = CancelToken::new();
        let bounded = host.request_cancellable(
            "bookmarks.create",
            json!({"details": {"title": "x", "url": "https://example.com/"}}),
            None,
            &cancel,
        );
        assert_eq!(bounded.err(), Some(protocol("BOUNDED_TRANSPORT")));
        assert_eq!(cancel.waiters(), 0);
        respond(&pipe, &sent, json!({"id": "1", "title": "one"}));
        assert!(first.join().ok().is_some_and(|result| result.is_ok()));
        host.close();
    }

    #[test]
    fn request_with_an_already_aborted_signal_rejects_instead_of_hanging() {
        let mut options = HostOptions::new(EXTENSION_ID);
        options.command_timeout = Duration::from_secs(60);
        let (host, pipe) = connect(options);
        let cancel = CancelToken::new();
        cancel.cancel();
        let result = host.request_cancellable(
            "bookmarks.create",
            json!({"details": {"title": "x", "url": "https://example.com/"}}),
            None,
            &cancel,
        );
        assert_eq!(
            result.as_ref().err().and_then(BridgeError::code),
            Some("ABORT_ERR")
        );
        assert!(host.is_connected());
        assert!(pipe.nothing_sent());
        host.close();
    }

    #[test]
    fn abort_rejects_a_queued_mutation_and_disconnects_a_started_one_as_indeterminate() {
        let disconnects = Arc::new(Mutex::new(Vec::<BridgeError>::new()));
        let mut options = HostOptions::new(EXTENSION_ID);
        options.command_timeout = Duration::from_secs(60);
        options.on_disconnect = Some(Box::new({
            let disconnects = Arc::clone(&disconnects);
            move |error: &BridgeError| {
                disconnects
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(error.clone());
            }
        }));
        let (host, mut pipe) = connect(options);
        let started_cancel = CancelToken::new();
        let started = create(&host, "one", &started_cancel);
        let _ = pipe.read_one();
        let queued_cancel = CancelToken::new();
        let queued = create(&host, "two", &queued_cancel);
        // Wait (bounded, on the host condvar) until the second request is admitted and queued.
        {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut state = host.shared.lock();
            while state.admitted < 2 && Instant::now() < deadline {
                state = host
                    .shared
                    .changed
                    .wait_timeout(state, deadline.saturating_duration_since(Instant::now()))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            assert_eq!(state.admitted, 2);
        }
        queued_cancel.cancel();
        let queued = queued.join().ok().and_then(Result::err);
        assert_eq!(
            queued.as_ref().and_then(BridgeError::code),
            Some("ABORT_ERR")
        );
        assert!(host.is_connected(), "a queued abort leaves the bridge up");
        assert!(pipe.nothing_sent(), "the aborted mutation was never sent");

        started_cancel.cancel();
        let started = started.join().ok().and_then(Result::err);
        assert_eq!(
            started.as_ref().and_then(BridgeError::code),
            Some("ABORT_ERR")
        );
        assert!(host.wait_disconnected(Duration::from_secs(5)));
        let later = host.request(
            "bookmarks.create",
            json!({"details": {"title": "three"}}),
            None,
        );
        assert!(
            later
                .err()
                .is_some_and(|error| error.message().contains("indeterminate"))
        );
        host.close();
        let seen = disconnects
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        assert_eq!(seen.len(), 1, "onDisconnect fires once");
        assert_eq!(seen[0].code(), Some("ABORT_ERR"));
        assert!(seen[0].message().contains("indeterminate"));
    }

    #[test]
    fn command_queue_is_bounded() {
        let mut options = HostOptions::new(EXTENSION_ID);
        options.max_pending = 1;
        let (host, mut pipe) = connect(options);
        let first = std::thread::spawn({
            let host = host.clone();
            move || {
                host.request(
                    "bookmarks.create",
                    json!({"details": {"title": "one"}}),
                    None,
                )
            }
        });
        let sent = pipe.read_one();
        let bounded = host.request(
            "bookmarks.create",
            json!({"details": {"title": "two"}}),
            None,
        );
        assert_eq!(bounded.err(), Some(protocol("BOUNDED_TRANSPORT")));
        respond(&pipe, &sent, json!({"id": "1", "title": "one"}));
        assert!(first.join().ok().is_some_and(|r| r.is_ok()));
        host.close();
    }

    #[test]
    fn public_send_rejects_unknown_envelopes_and_bookmark_results_reject_unknown_fields() {
        let (host, _pipe) = connect(HostOptions::new(EXTENSION_ID));
        assert_eq!(
            host.send(&json!({"type": "bogus"})).err(),
            Some(protocol("invalid outbound message type"))
        );
        let result = json!({"id": "1", "title": "x", "dateLastUsed": 1, "folderType": "bookmarks-bar", "syncing": true, "futureChromeField": "rejected"});
        assert_eq!(
            encode_frame(&json!({"protocol": 1, "type": "response", "requestId": "abcdef0123456789", "name": "bookmarks.create", "ok": true, "result": result}), MAX_MESSAGE_BYTES).err(),
            Some(protocol("invalid outbound message"))
        );
        host.close();
        assert!(
            host.request("bookmarks.create", json!({"details": {"title": "x"}}), None)
                .err()
                .is_some_and(|error| error.message() == "closed")
        );
    }

    #[test]
    fn bookmark_result_schema_is_recursive_and_explicitly_strict() {
        let response = json!({"protocol": 1, "type": "response", "requestId": "abcdef0123456789", "name": "bookmarks.create", "ok": true, "result": {"id": "1", "parentId": "0", "index": 0, "title": "folder", "dateAdded": 1, "dateGroupModified": 2, "dateLastUsed": 3, "folderType": "bookmarks-bar", "syncing": false, "unmodifiable": "managed", "children": [{"id": "2", "title": "link", "url": "https://example.test/"}]}});
        assert!(response_valid(&response));
        let mut extra = response.clone();
        extra["result"]["futureChromeField"] = json!(true);
        assert!(!response_valid(&extra));
        let mut nested = response;
        nested["result"]["children"][0]["futureChromeField"] = json!(true);
        assert!(!response_valid(&nested));
    }

    #[test]
    fn protocol_error_responses_are_schema_valid_and_cannot_collide_with_commands() {
        let response = json!({"protocol": 1, "type": "response", "requestId": "abcdef0123456789", "name": "protocolError", "ok": false, "error": {"code": "UNKNOWN_COMMAND", "message": "invalid command"}});
        assert!(response_valid(&response));
        assert!(!response_valid(
            &json!({"protocol": 1, "type": "response", "requestId": "abcdef0123456789", "name": "protocolError", "ok": true, "result": {}})
        ));
        assert!(!COMMAND_NAMES.contains(&"protocolError"));
    }

    #[test]
    fn protocol_error_from_the_extension_disconnects_with_its_message() {
        let (host, pipe) = connect(HostOptions::new(EXTENSION_ID));
        pipe.write_raw(&json!({"protocol": 1, "type": "response", "requestId": "abcdef0123456789", "name": "protocolError", "ok": false, "error": {"code": "UNKNOWN_COMMAND", "message": "invalid command"}}));
        assert!(host.wait_disconnected(Duration::from_secs(5)));
        assert!(!host.is_connected());
        let errors =
            String::from_utf8_lossy(&pipe.errors.lock().unwrap_or_else(PoisonError::into_inner))
                .into_owned();
        assert!(errors.contains("invalid command"), "{errors}");
    }

    #[test]
    fn chrome_api_routes_negotiated_writes_and_preserves_unsupported_fallback() {
        let (host, mut pipe) = connect(HostOptions::new(EXTENSION_ID));
        let worker = std::thread::spawn({
            let host = host.clone();
            move || {
                ChromeBridgeApi {
                    bridge: Some(&host),
                }
                .bookmarks_create(json!({"title": "x"}))
            }
        });
        let sent = pipe.read_one();
        assert_eq!(sent["params"], json!({"details": {"title": "x"}}));
        respond(&pipe, &sent, json!({"id": "2", "title": "x"}));
        assert_eq!(
            worker.join().ok().and_then(Result::ok),
            Some(json!({"id": "2", "title": "x"}))
        );
        let unsupported = ChromeBridgeApi {
            bridge: Some(&host),
        }
        .history_delete_url(json!({"url": "https://x.test/"}));
        assert!(
            unsupported
                .err()
                .is_some_and(|e| e.message().contains("requires the extension bridge"))
        );
        assert_eq!(
            chrome_event_wire("notifications", "onShown").as_deref(),
            Some("notifications.shown")
        );
        assert_eq!(
            chrome_event_wire("windows", "onFocusChanged").as_deref(),
            Some("windows.focusChanged")
        );
        assert_eq!(chrome_event_wire("tabs", "onRemoved"), None);
    }

    #[test]
    fn default_native_messaging_host_dirs() {
        let home = Path::new("/home/u");
        let expect = |browser: &str, platform: &str, rest: &str| {
            assert_eq!(
                default_native_messaging_host_dir(browser, platform, home).ok(),
                Some(home.join(rest))
            );
        };
        expect(
            "chrome",
            "darwin",
            "Library/Application Support/Google/Chrome/NativeMessagingHosts",
        );
        expect(
            "chromium",
            "darwin",
            "Library/Application Support/Chromium/NativeMessagingHosts",
        );
        expect(
            "chrome",
            "linux",
            ".config/google-chrome/NativeMessagingHosts",
        );
        expect("chromium", "linux", ".config/chromium/NativeMessagingHosts");
        assert!(
            default_native_messaging_host_dir("cloakbrowser", "linux", home)
                .err()
                .is_some_and(|e| e
                    .message()
                    .contains("custom browser requires nativeMessagingHostDir"))
        );
        assert!(
            default_native_messaging_host_dir("chrome", "win32", home)
                .err()
                .is_some_and(|e| e.message().contains("nativeMessagingHostDir"))
        );
    }

    #[cfg(unix)]
    mod registration {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn executable(dir: &Path) -> PathBuf {
            let path = dir.join("host");
            let _ = std::fs::write(&path, "#!/bin/sh\n");
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
            path
        }

        fn root() -> tempfile::TempDir {
            tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"))
        }

        #[test]
        fn registration_writes_pinned_manifest_atomically_and_rejects_unverified_cloakbrowser_path()
        {
            let root = root();
            let mut options = InstallOptions::new(executable(root.path()), EXTENSION_ID);
            options.native_messaging_host_dir = Some(root.path().join("hosts"));
            let (path, manifest) = install_native_messaging_host(&options, root.path())
                .unwrap_or_else(|e| panic!("{e}"));
            let on_disk: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_default())
                    .unwrap_or_default();
            assert_eq!(on_disk, manifest);
            assert_eq!(
                manifest["allowed_origins"][0],
                format!("chrome-extension://{EXTENSION_ID}/")
            );
            let mode = std::fs::metadata(&path).map(|m| m.permissions().mode() & 0o777);
            assert_eq!(mode.ok(), Some(0o600));
            let mut cloak = InstallOptions::new(executable(root.path()), EXTENSION_ID);
            cloak.browser = "cloakbrowser".into();
            assert!(
                install_native_messaging_host(&cloak, root.path())
                    .err()
                    .is_some_and(|e| e.message().contains("explicit and verified"))
            );
        }

        #[test]
        fn registration_rejects_endpoint_path_unsafe_host_names_and_overwrites_only_with_force() {
            let root = root();
            let mut options = InstallOptions::new(executable(root.path()), EXTENSION_ID);
            options.native_messaging_host_dir = Some(root.path().join("hosts"));
            let mut endpoint = options.clone();
            endpoint.endpoint_path = Some("/tmp/x".into());
            assert!(
                install_native_messaging_host(&endpoint, root.path())
                    .err()
                    .is_some_and(|e| e.message().contains("endpointPath"))
            );
            let mut evil = options.clone();
            evil.host_name = "../evil".into();
            assert!(
                install_native_messaging_host(&evil, root.path())
                    .err()
                    .is_some_and(|e| e.message().contains("hostName"))
            );
            assert!(install_native_messaging_host(&options, root.path()).is_ok());
            assert!(
                install_native_messaging_host(&options, root.path())
                    .err()
                    .is_some_and(|e| e.message().contains("already exists"))
            );
            options.force = true;
            assert!(install_native_messaging_host(&options, root.path()).is_ok());
        }

        #[test]
        fn registration_rejects_writable_or_symlinked_directory_components() {
            let root = root();
            let host = executable(root.path());
            let parent = root.path().join("parent");
            let _ = std::fs::create_dir(&parent);
            let _ = std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777));
            let mut options = InstallOptions::new(&host, EXTENSION_ID);
            options.native_messaging_host_dir = Some(parent.join("hosts"));
            assert!(
                install_native_messaging_host(&options, root.path())
                    .err()
                    .is_some_and(|e| e.message().contains("security rejected"))
            );
            let _ = std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700));
            let linked = root.path().join("linked");
            let _ = std::os::unix::fs::symlink(&parent, &linked);
            options.native_messaging_host_dir = Some(linked.join("hosts"));
            assert!(
                install_native_messaging_host(&options, root.path())
                    .err()
                    .is_some_and(|e| e.message().contains("security rejected"))
            );
        }

        #[test]
        fn registration_rejects_symlinked_executables() {
            let root = root();
            let real = root.path().join("real");
            let _ = std::fs::create_dir(&real);
            let host = executable(&real);
            let linked = root.path().join("linked-host");
            let _ = std::os::unix::fs::symlink(&host, &linked);
            let mut options = InstallOptions::new(&linked, EXTENSION_ID);
            options.native_messaging_host_dir = Some(root.path().join("hosts"));
            assert!(
                install_native_messaging_host(&options, root.path())
                    .err()
                    .is_some_and(|e| e.message().contains("owned executable"))
            );
        }
    }
}
