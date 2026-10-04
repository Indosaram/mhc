//! Port of `modes/interactive/interactive-host-runtime.ts`.
//!
//! Consumes `maho_rpc::rpc_client::RpcClient` and `maho_rpc::host_ensure::ensure_host`
//! (todo 36). senpi's `InteractiveSession` is a union of `AgentSession` and the remote proxy;
//! Rust needs an `InteractiveSession` trait for that substitution, specified separately in
//! `.omo/evidence/task-35-host-runtime-increment.md` rather than invented here.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use maho_rpc::custom_capability::{QUESTION_CAPABILITY, RENDERED_COMPONENTS_CAPABILITY};
use maho_rpc::host_ensure::{ensure_host, EnsureHostError, EnsureHostOptions, EnsuredHostInfo};
use maho_rpc::rpc_client::{
    classify_rpc_client_event, default_rpc_socket_path, is_provider_account_event, RpcClient,
    RpcClientEventKind, RpcClientOptions, RpcClientUnsubscribe, RpcClientError, RpcEventListener,
};

/// senpi's `HOST_CLIENT_CAPABILITIES`.
pub const HOST_CLIENT_CAPABILITIES: [&str; 2] = [RENDERED_COMPONENTS_CAPABILITY, QUESTION_CAPABILITY];

pub const INTERACTIVE_HOST_FALLBACK_WARNING: &str =
    "Warning: shared interactive host unavailable; continuing locally";
pub const INTERACTIVE_HOST_RECONNECTING_WARNING: &str =
    "Warning: shared interactive host connection lost; reconnecting";

/// senpi's reconnect attempt budget before `enterFallback`.
pub const RECONNECT_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveHostWarningKind {
    InteractiveHostFallback,
    InteractiveHostActionFailed,
}

/// senpi's `InteractiveHostWarning`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveHostWarning {
    pub kind: InteractiveHostWarningKind,
    pub message: String,
    pub cause: String,
}

pub type InteractiveHostWarningHandler = Arc<dyn Fn(InteractiveHostWarning) + Send + Sync>;

/// senpi's `InteractiveHostUiHandler`.
pub type HostUiHandler = Arc<dyn Fn(&Value) -> Option<Value> + Send + Sync>;

/// senpi's injectable `options.ensureHost`.
pub type EnsureHostFn =
    Arc<dyn Fn(EnsureHostRequest) -> Result<EnsuredHostInfo, String> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsureHostRequest {
    pub socket: String,
    pub agent_dir: Option<PathBuf>,
}

impl EnsureHostRequest {
    pub async fn run(&self) -> Result<EnsuredHostInfo, EnsureHostError> {
        ensure_host(EnsureHostOptions {
            socket: self.socket.clone(),
            agent_dir: self.agent_dir.clone(),
            ..Default::default()
        })
        .await
    }
}

/// senpi's `InteractiveHostRuntimeOptions`.
#[derive(Clone, Default)]
pub struct InteractiveHostRuntimeOptions {
    pub socket: String,
    pub agent_dir: Option<PathBuf>,
    pub ensure_host: Option<EnsureHostFn>,
    pub on_warning: Option<InteractiveHostWarningHandler>,
}

/// The reads `createInteractiveHostRuntime` takes off `AgentSessionRuntime`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSessionBinding {
    pub session_path: Option<String>,
    pub cwd: String,
    pub agent_dir: PathBuf,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking_level: Option<String>,
}

/// senpi's `RpcSessionState`, restricted to the fields the interactive proxy reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemoteSessionState {
    pub model: Option<Value>,
    pub thinking_level: Option<String>,
    pub thinking_selection: Option<Value>,
    pub last_abort_source: Option<String>,
    pub service_tier: Option<String>,
    pub fast_mode: bool,
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub retry_attempt: f64,
    pub is_bash_running: bool,
    pub steering_mode: Option<String>,
    pub follow_up_mode: Option<String>,
    pub session_file: Option<String>,
    pub session_id: Option<String>,
    pub session_name: Option<String>,
    pub cwd: Option<String>,
    pub project_trusted: bool,
    pub entries: Option<Value>,
    pub favorite_models: Value,
    pub scoped_models: Value,
    pub steering: Vec<String>,
    pub follow_up: Vec<String>,
    pub ordered: Vec<Value>,
    pub auto_compaction_enabled: bool,
    pub message_count: f64,
    pub pending_message_count: f64,
    pub usage_totals: Option<Value>,
    pub context_usage: Option<Value>,
}

impl RemoteSessionState {
    /// senpi's `stateFromRpc`.
    pub fn from_rpc(state: &Value) -> Self {
        Self {
            model: present(state, "model"),
            thinking_level: string_field(state, "thinkingLevel"),
            thinking_selection: present(state, "thinkingSelection"),
            last_abort_source: string_field(state, "lastAbortSource"),
            service_tier: string_field(state, "serviceTier"),
            fast_mode: bool_field(state, "fastMode"),
            is_streaming: bool_field(state, "isStreaming"),
            is_compacting: bool_field(state, "isCompacting"),
            retry_attempt: number_field(state, "retryAttempt"),
            is_bash_running: bool_field(state, "isBashRunning"),
            steering_mode: string_field(state, "steeringMode"),
            follow_up_mode: string_field(state, "followUpMode"),
            session_file: string_field(state, "sessionFile"),
            session_id: string_field(state, "sessionId"),
            session_name: string_field(state, "sessionName"),
            cwd: string_field(state, "cwd"),
            project_trusted: bool_field(state, "projectTrusted"),
            entries: present(state, "entries"),
            favorite_models: state.get("favoriteModels").cloned().unwrap_or(Value::Null),
            scoped_models: state.get("scopedModels").cloned().unwrap_or(Value::Null),
            steering: string_list(state, "steering"),
            follow_up: string_list(state, "followUp"),
            ordered: state.get("ordered").and_then(Value::as_array).cloned().unwrap_or_default(),
            auto_compaction_enabled: bool_field(state, "autoCompactionEnabled"),
            message_count: number_field(state, "messageCount"),
            pending_message_count: number_field(state, "pendingMessageCount"),
            usage_totals: present(state, "usageTotals"),
            context_usage: present(state, "contextUsage"),
        }
    }

    /// senpi's `nextQueuedInputOrder` seed value.
    pub fn next_queued_input_order(&self) -> f64 {
        self.ordered
            .iter()
            .filter_map(|item| item.get("enqueueOrder").and_then(Value::as_f64))
            .fold(0., f64::max)
    }
}

fn present(value: &Value, key: &str) -> Option<Value> {
    value.get(key).filter(|value| !value.is_null()).cloned()
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn bool_field(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn number_field(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(0.)
}

fn string_list(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

/// senpi's `hydrateMessageUpdate`; mutates the mirrored streaming message in place.
pub fn hydrate_message_update(event: &Value, streaming: Option<&mut Value>) -> Value {
    let Some(streaming) = streaming else { return event.clone(); };
    if event.get("type").and_then(Value::as_str) != Some("message_update") {
        return event.clone();
    }
    let Some(update) = event.get("assistantMessageEvent") else { return event.clone(); };
    let kind = update.get("type").and_then(Value::as_str).unwrap_or_default();
    if !matches!(kind, "text_delta" | "thinking_delta" | "toolcall_delta") {
        return event.clone();
    }
    let index = update.get("contentIndex").and_then(Value::as_u64).unwrap_or(0) as usize;
    let delta = update.get("delta").and_then(Value::as_str).unwrap_or_default();
    if let Some(block) = streaming.get_mut("content").and_then(|content| content.get_mut(index)) {
        match (kind, block.get("type").and_then(Value::as_str)) {
            ("text_delta", Some("text")) => append_str(block, "text", delta),
            ("thinking_delta", Some("thinking")) => append_str(block, "thinking", delta),
            ("toolcall_delta", Some("toolCall")) => {
                // senpi appends the delta to the JSON text of the accumulated arguments and re-parses
                // it. A toolcall starts with `arguments: {}` and its deltas carry the JSON text of the
                // real arguments, so an empty object is an empty buffer: parse the delta alone there.
                let raw = match block.get("arguments") {
                    None => delta.to_owned(),
                    Some(Value::Object(arguments)) if arguments.is_empty() => delta.to_owned(),
                    Some(existing) => format!("{}{delta}", Value::to_string(existing)),
                };
                if let Ok(parsed) = serde_json::from_str::<Value>(&raw)
                    && parsed.is_object()
                {
                    block["arguments"] = parsed;
                }
            }
            _ => {}
        }
    }
    if let Some(usage) = event.get("usage") {
        streaming["usage"] = usage.clone();
    }
    let mut hydrated = event.clone();
    hydrated["message"] = streaming.clone();
    if let Some(update) = hydrated.get_mut("assistantMessageEvent") {
        update["partial"] = streaming.clone();
    }
    hydrated
}

fn append_str(block: &mut Value, key: &str, delta: &str) {
    let existing = block.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    block[key] = Value::String(format!("{existing}{delta}"));
}

/// senpi's `isInteractiveHostEvent`.
pub fn is_interactive_host_event(event: &Value) -> bool {
    event.is_object() && event.get("type").and_then(Value::as_str).is_some()
}

/// senpi's `performRefresh` reconciliation; `None` means the mirror already matches.
pub fn reconcile_entries(
    authoritative: &[Value],
    mirror: &[Value],
    ids_at_refresh_start: &std::collections::BTreeSet<String>,
) -> Option<Vec<Value>> {
    if authoritative.is_empty() {
        return None;
    }
    if mirror.len() == authoritative.len()
        && mirror.iter().zip(authoritative).all(|(left, right)| left.get("id") == right.get("id"))
    {
        return None;
    }
    let mut rebuilt = authoritative.to_vec();
    let authoritative_ids: std::collections::BTreeSet<String> = authoritative
        .iter()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    for entry in mirror {
        let id = entry.get("id").and_then(Value::as_str).unwrap_or_default();
        if !ids_at_refresh_start.contains(id) && !authoritative_ids.contains(id) {
            rebuilt.push(entry.clone());
        }
    }
    Some(rebuilt)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRuntimeState {
    Connected,
    Reconnecting,
    Fallback,
    Disposed,
}

/// senpi's `createRemoteSessionProxy` state: the mirrored session state, the queued-input
/// order counter, the host UI handler and its pending-request queue, and the transport
/// subscription that feeds wire events in.
pub struct RemoteSessionProxy {
    client: Arc<tokio::sync::Mutex<RpcClient>>,
    state: Arc<Mutex<RemoteSessionState>>,
    listeners: Arc<Mutex<Vec<RpcEventListener>>>,
    session_listeners: Arc<Mutex<Vec<SessionEventListener>>>,
    host_ui_handler: Arc<Mutex<Option<HostUiHandler>>>,
    pending_ui_requests: Arc<Mutex<Vec<Value>>>,
    pending_ui_responses: Arc<Mutex<Vec<Value>>>,
    next_queued_input_order: Arc<Mutex<f64>>,
    streaming_assistant: Arc<Mutex<Option<Value>>>,
    subscription: Mutex<Option<RpcClientUnsubscribe>>,
    on_warning: Option<InteractiveHostWarningHandler>,
}

impl RemoteSessionProxy {
    pub fn new(state: RemoteSessionState, client: Arc<tokio::sync::Mutex<RpcClient>>, on_warning: Option<InteractiveHostWarningHandler>) -> Self {
        let next_order = state.next_queued_input_order();
        Self {
            client,
            state: Arc::new(Mutex::new(state)),
            listeners: Arc::new(Mutex::new(Vec::new())),
            session_listeners: Arc::new(Mutex::new(Vec::new())),
            host_ui_handler: Arc::new(Mutex::new(None)),
            pending_ui_requests: Arc::new(Mutex::new(Vec::new())),
            pending_ui_responses: Arc::new(Mutex::new(Vec::new())),
            next_queued_input_order: Arc::new(Mutex::new(next_order)),
            streaming_assistant: Arc::new(Mutex::new(None)),
            subscription: Mutex::new(None),
            on_warning,
        }
    }

    pub fn state(&self) -> RemoteSessionState {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// senpi's `performRefresh` state commit.
    pub fn set_state(&self, state: RemoteSessionState) {
        *self.next_queued_input_order.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = state.next_queued_input_order();
        *self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = state;
    }

    /// Keeps the transport subscription alive for as long as the proxy is.
    pub fn hold_subscription(&self, subscription: RpcClientUnsubscribe) {
        *self.subscription.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(subscription);
    }

    pub fn set_host_ui_handler(&self, handler: Option<HostUiHandler>) {
        let pending = handler
            .as_ref()
            .map(|_| std::mem::take(&mut *self.pending_ui_requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner)));
        *self.host_ui_handler.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = handler.clone();
        if let (Some(handler), Some(pending)) = (handler, pending) {
            for request in pending {
                self.queue_ui_response(&handler, &request);
            }
        }
    }

    /// senpi's `setHostUiHandler` drain of responses the mode produced.
    pub fn take_ui_responses(&self) -> Vec<Value> {
        std::mem::take(&mut *self.pending_ui_responses.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    fn queue_ui_response(&self, handler: &HostUiHandler, request: &Value) {
        if let Some(response) = handler(request) {
            self.pending_ui_responses.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(response);
        }
    }

    /// senpi's `subscribe`.
    pub fn subscribe(&self, listener: RpcEventListener) -> RemoteSubscription {
        self.listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(listener.clone());
        RemoteSubscription { listeners: self.listeners.clone(), listener: Some(listener) }
    }

    /// Fetch the host's authoritative message history off the transport, delivering it
    /// generation-tagged to `sender`. Spawned so the synchronous UI loop is never blocked; a task
    /// that finishes after a newer request or a replacement is discarded by the caller's generation
    /// check.
    pub fn spawn_fetch_messages(&self, generation: u64, sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<Value>, String>)>) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else { return; };
        let client = self.client.clone();
        handle.spawn(async move {
            let result = {
                let mut client = client.lock().await;
                client
                    .get_messages()
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|data| data.get("messages").and_then(Value::as_array).cloned().ok_or_else(|| "RPC get_messages returned no messages array".to_owned()))
            };
            let _ = sender.send((generation, result));
        });
    }

    /// Fetch the host's available-model catalog off the transport, generation-tagged like the
    /// history fetch, so the model selectors read the remote catalog without a stale local value.
    pub fn spawn_fetch_models(&self, generation: u64, sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<Value>, String>)>) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else { return; };
        let client = self.client.clone();
        handle.spawn(async move {
            let result = {
                let mut client = client.lock().await;
                client
                    .get_available_models()
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|data| data.get("models").and_then(Value::as_array).cloned().ok_or_else(|| "RPC get_available_models returned no models array".to_owned()))
            };
            let _ = sender.send((generation, result));
        });
    }

    /// senpi's `clearQueue` proxy arm: fire the RPC clear without awaiting; the caller reads the
    /// mirrored queue synchronously.
    pub fn spawn_clear_queue(&self, abort_will_follow: bool) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else { return; };
        let client = self.client.clone();
        handle.spawn(async move { let _ = client.lock().await.clear_queue(Some(abort_will_follow)).await; });
    }

    /// Fetch the host's session stats off the transport, generation-tagged.
    pub fn spawn_fetch_stats(&self, generation: u64, sender: tokio::sync::mpsc::UnboundedSender<(u64, Result<Value, String>)>) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else { return; };
        let client = self.client.clone();
        handle.spawn(async move {
            let result = { let mut client = client.lock().await; client.get_session_stats().await.map_err(|error| error.to_string()) };
            let _ = sender.send((generation, result));
        });
    }

    /// Subscribe to decoded `AgentSessionEvent`s; the returned guard removes the listener on drop.
    pub fn subscribe_session_events(&self, listener: SessionEventListener) -> SessionSubscription {
        self.session_listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(listener.clone());
        SessionSubscription { listeners: self.session_listeners.clone(), listener: Some(listener) }
    }

    /// senpi's `handleWireEvent`: hydrate, mirror the state, and fan out to listeners.
    pub fn handle_wire_event(&self, event: &Value) {
        if !is_interactive_host_event(event) {
            return;
        }
        match classify_rpc_client_event(event) {
            Some(RpcClientEventKind::ExtensionUiRequest) => {
                let handler = self.host_ui_handler.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
                match handler {
                    Some(handler) => self.queue_ui_response(&handler, event),
                    None => self.pending_ui_requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(event.clone()),
                }
                return;
            }
            Some(RpcClientEventKind::MessageStart) => {
                if event.get("message").and_then(|message| message.get("role")).and_then(Value::as_str) == Some("assistant") {
                    *self.streaming_assistant.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = event.get("message").cloned();
                }
            }
            Some(RpcClientEventKind::MessageEnd) => {
                if event.get("message").and_then(|message| message.get("role")).and_then(Value::as_str) == Some("assistant") {
                    *self.streaming_assistant.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = event.get("message").cloned();
                }
            }
            _ => {}
        }
        self.apply_state_event(event);
        let hydrated = {
            let mut streaming = self.streaming_assistant.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            hydrate_message_update(event, streaming.as_mut())
        };
        let listeners = self.listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        for listener in listeners {
            listener(&hydrated);
        }
        if let Some(session_event) = maho_rpc::session_event_decode::session_event_from_record(event) {
            let listeners = self.session_listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            for listener in listeners {
                listener(&session_event);
            }
        }
    }

    /// senpi's `handleWireEvent` state mirroring.
    pub fn apply_state_event(&self, event: &Value) {
        let kind = classify_rpc_client_event(event);
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match kind {
            Some(RpcClientEventKind::AgentStart) => state.is_streaming = true,
            Some(RpcClientEventKind::AgentSettled) => {
                state.is_streaming = false;
                state.retry_attempt = 0.;
            }
            Some(RpcClientEventKind::CompactionStart) => state.is_compacting = true,
            Some(RpcClientEventKind::CompactionEnd) => state.is_compacting = false,
            Some(RpcClientEventKind::AutoRetryStart) => state.retry_attempt = number_field(event, "attempt"),
            Some(RpcClientEventKind::AutoRetryEnd) => state.retry_attempt = 0.,
            Some(RpcClientEventKind::BashStart) => state.is_bash_running = true,
            Some(RpcClientEventKind::BashEnd) => state.is_bash_running = false,
            Some(RpcClientEventKind::QueueUpdate) => {
                if let Some(ordered) = event.get("ordered").and_then(Value::as_array) {
                    state.ordered = ordered.clone();
                }
                state.steering = string_list(event, "steering");
                state.follow_up = string_list(event, "followUp");
                state.pending_message_count = (state.steering.len() + state.follow_up.len()) as f64;
            }
            Some(RpcClientEventKind::ModelChanged) => {
                state.model = event.get("model").cloned();
                state.thinking_level = string_field(event, "thinkingLevel");
            }
            Some(RpcClientEventKind::ThinkingLevelChanged) => state.thinking_level = string_field(event, "level"),
            Some(RpcClientEventKind::ServiceTierChanged) => {
                state.service_tier = string_field(event, "tier");
                state.fast_mode = bool_field(event, "fastMode");
            }
            Some(RpcClientEventKind::SessionSettingsChanged) => {
                state.steering_mode = string_field(event, "steeringMode");
                state.follow_up_mode = string_field(event, "followUpMode");
                state.auto_compaction_enabled = bool_field(event, "autoCompactionEnabled");
            }
            Some(RpcClientEventKind::SessionInfoChanged) => state.session_name = string_field(event, "name"),
            _ => {}
        }
        let order = state.next_queued_input_order();
        *self.next_queued_input_order.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = order;
    }

    /// senpi's `reserveQueuedInputOrder`.
    pub fn reserve_queued_input_order(&self) -> f64 {
        let mut order = self.next_queued_input_order.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *order += 1.;
        *order
    }

    /// senpi's `reportActionFailure`.
    pub fn report_action_failure(&self, action: &str, error: &RpcClientError) {
        if error.is_transport_gone() {
            return;
        }
        if let Some(handler) = &self.on_warning {
            handler(InteractiveHostWarning {
                kind: InteractiveHostWarningKind::InteractiveHostActionFailed,
                message: format!("Warning: shared interactive host {action} failed: {error}"),
                cause: error.to_string(),
            });
        }
    }

    // ---- senpi's `createRemoteSessionProxy` client surface --------------------------------
    //
    // The transport-facing AgentSession members senpi's proxy overrides, ported as inherent
    // methods over the shared `RpcClient` and the mirrored state. The members that read or
    // mutate the LOCAL AgentSession (messages, entries, sessionManager, `executeBash` with
    // injected operations, `createReplacedSessionContext`, `getLastAssistantText`) stay in the
    // `InteractiveSession` seam recorded in `.omo/evidence/task-35-host-runtime-increment.md`.

    /// senpi's `transportCall` fallback: a transport loss is reported and yields the cancelled
    /// value; a command refusal propagates to the caller.
    fn settle<T>(&self, action: &str, result: Result<T, RpcClientError>, cancelled: T) -> Result<T, RpcClientError> {
        match result {
            Ok(value) => Ok(value),
            Err(error) if error.is_transport_gone() => {
                self.report_action_failure(action, &error);
                Ok(cancelled)
            }
            Err(error) => Err(error),
        }
    }

    /// senpi's fire-and-forget setters: report a transport loss, propagate a refusal.
    fn settle_unit(&self, action: &str, result: Result<(), RpcClientError>) -> Result<(), RpcClientError> {
        self.settle(action, result, ())
    }

    fn read<R>(&self, f: impl FnOnce(&RemoteSessionState) -> R) -> R {
        f(&self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    fn update<R>(&self, f: impl FnOnce(&mut RemoteSessionState) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    // senpi's mirrored reads (`isStreaming`, `sessionId`, `model`, ...).
    pub fn is_streaming(&self) -> bool { self.read(|state| state.is_streaming) }
    pub fn is_idle(&self) -> bool { !self.is_streaming() }
    pub fn is_compacting(&self) -> bool { self.read(|state| state.is_compacting) }
    pub fn pending_message_count(&self) -> f64 { self.read(|state| state.pending_message_count) }
    pub fn retry_attempt(&self) -> f64 { self.read(|state| state.retry_attempt) }
    pub fn is_bash_running(&self) -> bool { self.read(|state| state.is_bash_running) }
    pub fn is_fast_mode_active(&self) -> bool { self.read(|state| state.fast_mode) }
    pub fn session_file(&self) -> Option<String> { self.read(|state| state.session_file.clone()) }
    pub fn session_id(&self) -> Option<String> { self.read(|state| state.session_id.clone()) }
    pub fn session_name(&self) -> Option<String> { self.read(|state| state.session_name.clone()) }
    pub fn service_tier(&self) -> Option<String> { self.read(|state| state.service_tier.clone()) }
    pub fn steering_mode(&self) -> Option<String> { self.read(|state| state.steering_mode.clone()) }
    pub fn follow_up_mode(&self) -> Option<String> { self.read(|state| state.follow_up_mode.clone()) }
    pub fn auto_compaction_enabled(&self) -> bool { self.read(|state| state.auto_compaction_enabled) }
    pub fn model(&self) -> Option<Value> { self.read(|state| state.model.clone()) }
    pub fn thinking_level(&self) -> Option<String> { self.read(|state| state.thinking_level.clone()) }
    pub fn context_usage(&self) -> Option<Value> { self.read(|state| state.context_usage.clone()) }
    pub fn favorite_models(&self) -> Value { self.read(|state| state.favorite_models.clone()) }
    pub fn scoped_models(&self) -> Value { self.read(|state| state.scoped_models.clone()) }
    pub fn steering_messages(&self) -> Vec<String> { self.read(|state| state.steering.clone()) }
    pub fn follow_up_messages(&self) -> Vec<String> { self.read(|state| state.follow_up.clone()) }

    /// senpi's `prompt`.
    pub async fn prompt(&self, message: &str, options: Value) -> Result<(), RpcClientError> {
        let result = self.client.lock().await.prompt(message, options, |_| {}, |_| {}).await;
        self.settle_unit("prompt", result)
    }

    /// senpi's `abort`.
    pub async fn abort(&self) -> Result<(), RpcClientError> {
        let result = self.client.lock().await.abort().await;
        self.settle_unit("abort", result)
    }

    /// senpi's `abortCompaction`.
    pub async fn abort_compaction(&self) -> Result<(), RpcClientError> {
        let result = self.client.lock().await.abort_compaction().await;
        self.settle_unit("abortCompaction", result)
    }

    /// senpi's `abortBranchSummary`.
    pub async fn abort_branch_summary(&self) -> Result<(), RpcClientError> {
        let result = self.client.lock().await.abort_branch_summary().await;
        self.settle_unit("abortBranchSummary", result)
    }

    /// senpi's `abortRetry`.
    pub async fn abort_retry(&self) -> Result<(), RpcClientError> {
        let result = self.client.lock().await.abort_retry().await;
        self.settle_unit("abortRetry", result)
    }

    /// senpi's `abortBash`.
    pub async fn abort_bash(&self) -> Result<(), RpcClientError> {
        let result = self.client.lock().await.abort_bash().await;
        self.settle_unit("abortBash", result)
    }

    /// senpi's `waitForIdle`.
    pub async fn wait_for_idle(&self, timeout: std::time::Duration) -> Result<(), RpcClientError> {
        let result = self.client.lock().await.wait_for_idle(timeout).await;
        self.settle_unit("waitForIdle", result)
    }

    /// senpi's `steer`: the mirror records the queued input before the frame goes out.
    pub async fn steer(&self, message: &str, images: Option<Value>, enqueue_order: Option<f64>) -> Result<(), RpcClientError> {
        let order = enqueue_order.unwrap_or_else(|| self.read(|state| state.next_queued_input_order()) + 1.);
        self.update(|state| {
            state.steering.push(message.to_owned());
            state.ordered.push(json!({"text": message, "mode": "steer", "enqueueOrder": order}));
            state.pending_message_count += 1.;
        });
        let result = self.client.lock().await.steer(message, images, Some(order)).await;
        self.settle_unit("steer", result)
    }

    /// senpi's `followUp`.
    pub async fn follow_up(&self, message: &str, images: Option<Value>, enqueue_order: Option<f64>) -> Result<(), RpcClientError> {
        let order = enqueue_order.unwrap_or_else(|| self.read(|state| state.next_queued_input_order()) + 1.);
        self.update(|state| {
            state.follow_up.push(message.to_owned());
            state.ordered.push(json!({"text": message, "mode": "followUp", "enqueueOrder": order}));
            state.pending_message_count += 1.;
        });
        let result = self.client.lock().await.follow_up(message, images, Some(order)).await;
        self.settle_unit("followUp", result)
    }

    /// senpi's `setModel` / `setSessionModel`.
    pub async fn set_model(&self, provider: &str, model_id: &str) -> Result<Value, RpcClientError> {
        let result = self.client.lock().await.set_model(provider, model_id).await;
        self.settle("setModel", result, Value::Null)
    }

    /// senpi's `cycleModel`.
    pub async fn cycle_model(&self, direction: &str) -> Result<Value, RpcClientError> {
        let result = self.client.lock().await.cycle_model(direction).await;
        self.settle("cycleModel", result, Value::Null)
    }

    /// senpi's `setThinkingLevel`.
    pub async fn set_thinking_level(&self, level: &str, scope: Option<&str>) -> Result<Value, RpcClientError> {
        let result = self.client.lock().await.set_thinking_level(level, scope).await;
        self.settle("setThinkingLevel", result, Value::Null)
    }

    /// senpi's `cycleThinkingLevel`.
    pub async fn cycle_thinking_level(&self) -> Result<Value, RpcClientError> {
        let result = self.client.lock().await.cycle_thinking_level().await;
        self.settle("cycleThinkingLevel", result, Value::Null)
    }

    /// senpi's `getAvailableThinkingLevels`.
    pub async fn get_available_thinking_levels(&self) -> Result<Value, RpcClientError> {
        let result = self.client.lock().await.get_available_thinking_levels().await;
        self.settle("getAvailableThinkingLevels", result, Value::Array(Vec::new()))
    }

    /// senpi's `setFastMode`.
    pub async fn set_fast_mode(&self, enabled: bool) -> Result<Value, RpcClientError> {
        let result = self.client.lock().await.set_fast_mode(enabled).await;
        self.settle("setFastMode", result, Value::Null)
    }

    /// senpi's `setAutoRetryEnabled`.
    pub async fn set_auto_retry(&self, enabled: bool) -> Result<(), RpcClientError> {
        let result = self.client.lock().await.set_auto_retry(enabled).await;
        self.settle_unit("setAutoRetryEnabled", result)
    }

    /// senpi's `setSteeringMode`.
    pub async fn set_steering_mode(&self, mode: &str) -> Result<(), RpcClientError> {
        let result = self.client.lock().await.set_steering_mode(mode).await;
        self.settle_unit("setSteeringMode", result)
    }

    /// senpi's `setFollowUpMode`.
    pub async fn set_follow_up_mode(&self, mode: &str) -> Result<(), RpcClientError> {
        let result = self.client.lock().await.set_follow_up_mode(mode).await;
        self.settle_unit("setFollowUpMode", result)
    }

    /// senpi's `setAutoCompactionEnabled`.
    pub async fn set_auto_compaction(&self, enabled: bool) -> Result<(), RpcClientError> {
        let result = self.client.lock().await.set_auto_compaction(enabled).await;
        self.settle_unit("setAutoCompaction", result)
    }

    /// senpi's `setFavoriteModels`: the mirror is updated before the frame goes out.
    pub async fn set_favorite_models(&self, models: Value) -> Result<(), RpcClientError> {
        self.update(|state| state.favorite_models = models.clone());
        let result = self.client.lock().await.set_favorite_models(models).await;
        self.settle_unit("setFavoriteModels", result)
    }

    /// senpi's `setScopedModels`.
    pub async fn set_scoped_models(&self, models: Value) -> Result<(), RpcClientError> {
        self.update(|state| state.scoped_models = models.clone());
        let result = self.client.lock().await.set_scoped_models(models).await;
        self.settle_unit("setScopedModels", result)
    }

    /// senpi's `setSessionName`.
    pub async fn set_session_name(&self, name: &str) -> Result<(), RpcClientError> {
        let result = self.client.lock().await.set_session_name(name).await;
        self.settle_unit("setSessionName", result)
    }

    /// senpi's `compact`.
    pub async fn compact(&self, instructions: Option<&str>) -> Result<Value, RpcClientError> {
        let result = self.client.lock().await.compact(instructions).await;
        self.settle("compact", result, Value::Null)
    }

    /// senpi's `navigateTree`.
    pub async fn navigate_tree(&self, target_id: &str, options: Value) -> Result<Value, RpcClientError> {
        let result = self.client.lock().await.navigate_tree(target_id, options).await;
        self.settle("navigateTree", result, json!({"cancelled": true}))
    }

    /// senpi's `reload`.
    pub async fn reload(&self) -> Result<Value, RpcClientError> {
        let result = self.client.lock().await.reload().await;
        self.settle("reload", result, json!({"cancelled": true}))
    }

    /// senpi's `checkReloadVeto`.
    pub async fn check_reload_veto(&self) -> Result<Value, RpcClientError> {
        let result = self.client.lock().await.check_reload_veto().await;
        self.settle("checkReloadVeto", result, json!({"cancelled": true}))
    }

    /// senpi's `getSessionStats`.
    pub async fn get_session_stats(&self) -> Result<Value, RpcClientError> {
        let result = self.client.lock().await.get_session_stats().await;
        self.settle("getSessionStats", result, Value::Null)
    }

    /// senpi's `getUserMessagesForForking`.
    pub async fn get_user_messages_for_forking(&self) -> Result<Value, RpcClientError> {
        let result = self.client.lock().await.get_fork_messages().await;
        self.settle("getUserMessagesForForking", result, Value::Array(Vec::new()))
    }

    /// senpi's `editAssistantMessage` proxy arm: forward the edit, then refresh the mirror.
    pub async fn edit_assistant_message(&self, entry_id: &str, text: &str, options: Value) -> Result<Value, RpcClientError> {
        let result = { self.client.lock().await.edit_assistant_message(entry_id, text, options).await? };
        self.refresh().await;
        Ok(result)
    }

    /// senpi's proxy `refresh()`: re-read the host session state into the mirror.
    pub async fn refresh(&self) {
        if let Ok(state) = self.client.lock().await.get_state().await {
            self.set_state(RemoteSessionState::from_rpc(&state));
        }
    }

    /// senpi's `executeBash` proxy arm (host-side execution; no local chunk callbacks here).
    pub async fn bash(&self, command: &str, options: Value) -> Result<Value, RpcClientError> {
        self.client.lock().await.bash(command, options).await
    }

    /// senpi's `exportToJsonl`.
    pub async fn export_jsonl(&self, output_path: Option<&str>) -> Result<Value, RpcClientError> {
        let result = self.client.lock().await.export_jsonl(output_path).await;
        self.settle("exportToJsonl", result, Value::Null)
    }

    /// senpi's `exportToHtml`.
    pub async fn export_html(&self, output_path: Option<&str>, theme_name: Option<&str>) -> Result<Value, RpcClientError> {
        let result = self.client.lock().await.export_html(output_path, theme_name).await;
        self.settle("exportToHtml", result, Value::Null)
    }

    /// senpi's `recordBashResult`.
    pub async fn record_bash_result(&self, command: &str, result: Value, exclude_from_context: Option<bool>) -> Result<(), RpcClientError> {
        let outcome = self.client.lock().await.record_bash_result(command, result, exclude_from_context).await;
        self.settle_unit("recordBashResult", outcome)
    }

    /// senpi's `appendLabelChange` (the remote SessionManager shim).
    pub async fn set_label(&self, entry_id: &str, label: Option<&str>) -> Result<(), RpcClientError> {
        let result = self.client.lock().await.set_label(entry_id, label).await;
        self.settle_unit("appendLabelChange", result)
    }

    /// senpi's `sendMessage` (`sendCustomMessage`).
    pub async fn send_custom_message(&self, message: Value, options: Option<Value>) -> Result<(), RpcClientError> {
        let result = self.client.lock().await.send_custom_message(message, options).await;
        self.settle_unit("sendMessage", result)
    }

    /// senpi's `clearQueue`: the mirrored snapshot is what the caller restores, and the frame
    /// only tells the host to drop its queue.
    pub async fn clear_queue(&self, abort_will_follow: Option<bool>) -> Value {
        let snapshot = self.read(|state| json!({
            "steering": state.steering,
            "followUp": state.follow_up,
            "ordered": state.ordered,
        }));
        let result = self.client.lock().await.clear_queue(abort_will_follow).await;
        if let Err(error) = result { self.report_action_failure("clearQueue", &error); }
        snapshot
    }
}

/// senpi's `subscribe` unsubscribe handle.
pub struct RemoteSubscription {
    listeners: Arc<Mutex<Vec<RpcEventListener>>>,
    listener: Option<RpcEventListener>,
}

/// A typed session-event listener: the proxy decodes each wire event into an
/// `AgentSessionEvent` (the inverse of the host's `session_event_record`) and forwards it.
pub type SessionEventListener = Arc<dyn Fn(&maho_ext_api::AgentSessionEvent) + Send + Sync>;

pub struct SessionSubscription {
    listeners: Arc<Mutex<Vec<SessionEventListener>>>,
    listener: Option<SessionEventListener>,
}

impl Drop for SessionSubscription {
    fn drop(&mut self) {
        let Some(listener) = self.listener.take() else { return; };
        self.listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|current| !Arc::ptr_eq(current, &listener));
    }
}

impl Drop for RemoteSubscription {
    fn drop(&mut self) {
        let Some(listener) = self.listener.take() else { return; };
        self.listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|current| !Arc::ptr_eq(current, &listener));
    }
}

pub type RebindSessionFn = Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

/// senpi's `RemoteInteractiveRuntime`.
pub struct RemoteInteractiveRuntime {
    client: Arc<tokio::sync::Mutex<RpcClient>>,
    proxy: Arc<RemoteSessionProxy>,
    binding: HostSessionBinding,
    state: Mutex<HostRuntimeState>,
    options: InteractiveHostRuntimeOptions,
    rebind_session: Mutex<Option<RebindSessionFn>>,
    before_session_invalidate: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    client_info_sent: Mutex<bool>,
    last_client_width: Mutex<f64>,
}

impl RemoteInteractiveRuntime {
    pub fn new(
        client: Arc<tokio::sync::Mutex<RpcClient>>,
        proxy: Arc<RemoteSessionProxy>,
        binding: HostSessionBinding,
        options: InteractiveHostRuntimeOptions,
    ) -> Self {
        Self {
            client,
            proxy,
            binding,
            state: Mutex::new(HostRuntimeState::Connected),
            options,
            rebind_session: Mutex::new(None),
            before_session_invalidate: Mutex::new(None),
            client_info_sent: Mutex::new(false),
            last_client_width: Mutex::new(80.),
        }
    }

    pub fn proxy(&self) -> &Arc<RemoteSessionProxy> {
        &self.proxy
    }

    pub fn binding(&self) -> &HostSessionBinding {
        &self.binding
    }

    pub fn state(&self) -> HostRuntimeState {
        *self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn is_reconnecting(&self) -> bool {
        self.state() == HostRuntimeState::Reconnecting
    }

    pub fn is_fallback(&self) -> bool {
        self.state() == HostRuntimeState::Fallback
    }

    fn warn(&self, kind: InteractiveHostWarningKind, message: &str, cause: String) {
        if let Some(handler) = &self.options.on_warning {
            handler(InteractiveHostWarning { kind, message: message.to_owned(), cause });
        }
    }

    /// senpi's `enterConnected`.
    pub fn enter_connected(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(*state, HostRuntimeState::Disposed | HostRuntimeState::Fallback) {
            *state = HostRuntimeState::Connected;
        }
    }

    /// senpi's `enterReconnecting`.
    pub fn enter_reconnecting(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(*state, HostRuntimeState::Disposed | HostRuntimeState::Fallback) {
            *state = HostRuntimeState::Reconnecting;
        }
    }

    /// senpi's `enterFallback`.
    pub async fn enter_fallback(&self, cause: String) {
        {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if matches!(*state, HostRuntimeState::Disposed | HostRuntimeState::Fallback) {
                return;
            }
            *state = HostRuntimeState::Fallback;
        }
        if let Some(callback) = self.before_session_invalidate.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() {
            callback();
        }
        let rebind = self.rebind_session.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if let Some(rebind) = rebind
            && let Err(error) = rebind().await
        {
            self.warn(InteractiveHostWarningKind::InteractiveHostActionFailed, &format!("Warning: {error}"), error);
        }
        self.warn(InteractiveHostWarningKind::InteractiveHostFallback, INTERACTIVE_HOST_FALLBACK_WARNING, cause);
    }

    /// senpi's `setRebindSession`.
    pub fn set_rebind_session(&self, callback: Option<RebindSessionFn>) {
        *self.rebind_session.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = callback;
    }

    /// senpi's `setBeforeSessionInvalidate`.
    pub fn set_before_session_invalidate(&self, callback: Option<Box<dyn Fn() + Send + Sync>>) {
        *self.before_session_invalidate.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = callback;
    }

    /// senpi's `setHostUiHandler`.
    pub fn set_host_ui_handler(&self, handler: Option<HostUiHandler>) {
        self.proxy.set_host_ui_handler(handler);
    }

    /// senpi's `sendHostUiProgress`.
    pub async fn send_host_ui_progress(&self, progress: Value) {
        let _ = self.client.lock().await.send_extension_ui_progress(progress).await;
    }

    /// senpi's `setClientInfo`; capabilities ride the first frame only.
    pub async fn set_client_info(&self, width: f64) {
        *self.last_client_width.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = width;
        let capabilities = {
            let mut sent = self.client_info_sent.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let capabilities = (!*sent).then(client_capabilities);
            *sent = true;
            capabilities
        };
        let _ = self.client.lock().await.set_client_info(width, capabilities).await;
    }

    /// senpi's `reRegisterClientInfo`.
    pub async fn re_register_client_info(&self) {
        let width = *self.last_client_width.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = self.client.lock().await.set_client_info(width, Some(client_capabilities())).await;
    }

    /// senpi's `newSession`.
    pub async fn new_session(&self, parent_session: Option<&str>) -> Result<bool, RpcClientError> {
        if self.is_fallback() {
            return Ok(true);
        }
        let result = { self.client.lock().await.new_session(parent_session).await? };
        if is_cancelled(&result) {
            return Ok(true);
        }
        self.after_replacement().await;
        Ok(false)
    }

    /// senpi's `switchSession`.
    pub async fn switch_session(&self, session_path: &str, cwd_override: Option<&str>) -> Result<bool, RpcClientError> {
        if self.is_fallback() {
            return Ok(true);
        }
        let result = { self.client.lock().await.switch_session(session_path, cwd_override).await? };
        if is_cancelled(&result) {
            return Ok(true);
        }
        self.after_replacement().await;
        Ok(false)
    }

    /// senpi's `fork`.
    pub async fn fork(&self, entry_id: &str, position: Option<&str>) -> Result<bool, RpcClientError> {
        if self.is_fallback() {
            return Ok(true);
        }
        let result = { self.client.lock().await.fork(entry_id, position).await? };
        if is_cancelled(&result) {
            return Ok(true);
        }
        self.after_replacement().await;
        Ok(false)
    }

    /// senpi's `importFromJsonl`.
    pub async fn import_jsonl(&self, input_path: &str, cwd_override: Option<&str>) -> Result<bool, RpcClientError> {
        if self.is_fallback() {
            return Ok(true);
        }
        let result = { self.client.lock().await.import_jsonl(input_path, cwd_override).await? };
        if is_cancelled(&result) {
            return Ok(true);
        }
        self.after_replacement().await;
        Ok(false)
    }

    async fn after_replacement(&self) {
        if let Some(callback) = self.before_session_invalidate.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() {
            callback();
        }
        if let Ok(state) = self.client.lock().await.get_state().await {
            self.proxy.set_state(RemoteSessionState::from_rpc(&state));
        }
        let rebind = self.rebind_session.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if let Some(rebind) = rebind
            && let Err(error) = rebind().await
        {
            self.warn(InteractiveHostWarningKind::InteractiveHostActionFailed, &format!("Warning: {error}"), error);
        }
    }

    /// senpi's `dispose`.
    pub async fn dispose(&self) {
        {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if *state == HostRuntimeState::Disposed {
                return;
            }
            *state = HostRuntimeState::Disposed;
        }
        let mut client = self.client.lock().await;
        let _ = client.close_session(None).await;
        client.stop().await;
    }

    /// senpi's `scheduleReconnect` loop.
    pub async fn reconnect(&self) -> bool {
        if self.is_fallback() || self.state() == HostRuntimeState::Disposed {
            return false;
        }
        self.enter_reconnecting();
        self.warn(InteractiveHostWarningKind::InteractiveHostActionFailed, INTERACTIVE_HOST_RECONNECTING_WARNING, String::new());
        let mut cause = String::new();
        for _ in 0..RECONNECT_ATTEMPTS {
            if self.state() == HostRuntimeState::Disposed {
                return false;
            }
            match self.try_reopen().await {
                Ok(()) => {
                    self.enter_connected();
                    return true;
                }
                Err(error) => {
                    cause = error;
                    self.client.lock().await.stop().await;
                }
            }
        }
        self.enter_fallback(cause).await;
        false
    }

    async fn try_reopen(&self) -> Result<(), String> {
        let request = EnsureHostRequest { socket: self.options.socket.clone(), agent_dir: self.options.agent_dir.clone() };
        match &self.options.ensure_host {
            Some(handler) => {
                handler(request)?;
            }
            None => {
                request.run().await.map_err(|error| error.to_string())?;
            }
        }
        let mut client = self.client.lock().await;
        client.start().await.map_err(|error| error.to_string())?;
        client.open_session(open_session_options(&self.binding)).await.map_err(|error| error.to_string())?;
        drop(client);
        let state = { self.client.lock().await.get_state().await.map_err(|error| error.to_string())? };
        self.proxy.set_state(RemoteSessionState::from_rpc(&state));
        Ok(())
    }
}

fn client_capabilities() -> Vec<String> {
    HOST_CLIENT_CAPABILITIES.iter().map(|value| (*value).to_owned()).collect()
}

fn is_cancelled(result: &Value) -> bool {
    result.get("cancelled").and_then(Value::as_bool) == Some(true)
}

fn open_session_options(binding: &HostSessionBinding) -> Value {
    json!({
        "sessionPath": binding.session_path,
        "cwd": binding.cwd,
        "provider": binding.provider,
        "modelId": binding.model,
        "thinkingLevel": binding.thinking_level,
    })
}

/// senpi's `createInteractiveHostRuntime` outcome.
pub enum InteractiveHostRuntimeOutcome {
    Remote(Arc<RemoteInteractiveRuntime>),
    Fallback(InteractiveHostWarning),
}

impl InteractiveHostRuntimeOutcome {
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Remote(_))
    }

    /// The session host to mount on `InteractiveMode` for a successful remote join; `None` when the
    /// mount fell back to the local session.
    pub fn session_host(&self) -> Option<Arc<dyn crate::interactive_session::InteractiveSession>> {
        match self {
            Self::Remote(runtime) => Some(runtime.clone() as Arc<dyn crate::interactive_session::InteractiveSession>),
            Self::Fallback(_) => None,
        }
    }
}

/// senpi's `createInteractiveHostRuntime`.
pub async fn create_interactive_host_runtime(
    binding: HostSessionBinding,
    options: InteractiveHostRuntimeOptions,
) -> InteractiveHostRuntimeOutcome {
    let warn_fallback = |cause: &str| InteractiveHostWarning {
        kind: InteractiveHostWarningKind::InteractiveHostFallback,
        message: INTERACTIVE_HOST_FALLBACK_WARNING.to_owned(),
        cause: cause.to_owned(),
    };
    if binding.session_path.is_none() {
        return InteractiveHostRuntimeOutcome::Fallback(warn_fallback("session has no file on disk"));
    }
    let socket = if options.socket.is_empty() {
        default_rpc_socket_path(&binding.agent_dir).to_string_lossy().into_owned()
    } else {
        options.socket.clone()
    };
    let ensure_request = EnsureHostRequest { socket: socket.clone(), agent_dir: options.agent_dir.clone() };
    let ensured = match &options.ensure_host {
        Some(handler) => handler(ensure_request.clone()).map(|_| ()),
        None => ensure_request.run().await.map(|_| ()).map_err(|error| error.to_string()),
    };
    if let Err(cause) = ensured {
        return InteractiveHostRuntimeOutcome::Fallback(warn_fallback(&cause));
    }

    let mut client = RpcClient::new(RpcClientOptions {
        socket_path: Some(PathBuf::from(&socket)),
        agent_dir: options.agent_dir.clone(),
        cwd: Some(binding.cwd.clone()),
        provider: binding.provider.clone(),
        model: binding.model.clone(),
        ..Default::default()
    });
    if let Err(error) = client.start().await {
        return InteractiveHostRuntimeOutcome::Fallback(warn_fallback(&error.to_string()));
    }
    if let Err(error) = client.set_client_info(80., Some(client_capabilities())).await {
        client.stop().await;
        return InteractiveHostRuntimeOutcome::Fallback(warn_fallback(&error.to_string()));
    }
    if let Err(error) = client.open_session(open_session_options(&binding)).await {
        client.stop().await;
        return InteractiveHostRuntimeOutcome::Fallback(warn_fallback(&error.to_string()));
    }
    let state = match client.get_state().await {
        Ok(state) => RemoteSessionState::from_rpc(&state),
        Err(error) => {
            client.stop().await;
            return InteractiveHostRuntimeOutcome::Fallback(warn_fallback(&error.to_string()));
        }
    };
    let client = Arc::new(tokio::sync::Mutex::new(client));
    let proxy = Arc::new(RemoteSessionProxy::new(state, client.clone(), options.on_warning.clone()));
    let subscription = client
        .lock()
        .await
        .on_event(Arc::new({
            let proxy = proxy.clone();
            move |event| proxy.handle_wire_event(event)
        }));
    proxy.hold_subscription(subscription);
    let runtime = Arc::new(RemoteInteractiveRuntime::new(client, proxy, binding, options));
    InteractiveHostRuntimeOutcome::Remote(runtime)
}

/// senpi's `isProviderAccountEvent` use: records that are not agent events.
pub fn is_connection_level_event(event: &Value) -> bool {
    is_provider_account_event(event)
}

/// The queue the footer renders off the resolved session state.
pub fn queue_snapshot(state: &RemoteSessionState) -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("steering".to_owned(), json!(state.steering)),
        ("followUp".to_owned(), json!(state.follow_up)),
        ("pendingMessageCount".to_owned(), json!(state.pending_message_count)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_value() -> Value {
        json!({
            "model": {"provider": "anthropic", "id": "claude"},
            "thinkingLevel": "high",
            "thinkingSelection": {"level": "high"},
            "lastAbortSource": "user",
            "serviceTier": "priority",
            "fastMode": true,
            "isStreaming": false,
            "isCompacting": false,
            "retryAttempt": 2,
            "isBashRunning": true,
            "steeringMode": "all",
            "followUpMode": "one-at-a-time",
            "sessionFile": "/sessions/one.jsonl",
            "sessionId": "one",
            "sessionName": "Named",
            "cwd": "/tmp",
            "projectTrusted": true,
            "autoCompactionEnabled": true,
            "messageCount": 4,
            "pendingMessageCount": 2,
            "steering": ["a"],
            "followUp": ["b"],
            "ordered": [{"text": "a", "mode": "steer", "enqueueOrder": 3}],
            "favoriteModels": [{"provider": "anthropic", "id": "claude"}],
            "scopedModels": [],
            "usageTotals": {"input": 1, "output": 2, "cacheRead": 0, "cacheWrite": 0, "cost": 0.5},
            "contextUsage": {"tokens": 10, "contextWindow": 100, "percent": 10.0},
            "entries": [{"type": "message", "id": "e1"}],
        })
    }

    fn binding(session_path: Option<&str>) -> HostSessionBinding {
        HostSessionBinding {
            session_path: session_path.map(str::to_owned),
            cwd: "/tmp".into(),
            agent_dir: PathBuf::from("/tmp/agent"),
            provider: None,
            model: None,
            thinking_level: None,
        }
    }

    fn proxy() -> RemoteSessionProxy {
        RemoteSessionProxy::new(RemoteSessionState::default(), detached_client(), None)
    }

    /// A client on a socket whose host end is dropped: reads and mirror-only tests never write,
    /// and a fire-and-forget write fails as a swallowed transport loss.
    fn detached_client() -> Arc<tokio::sync::Mutex<RpcClient>> {
        let (client_stream, _host) = tokio::net::UnixStream::pair().expect("socket pair");
        Arc::new(tokio::sync::Mutex::new(RpcClient::from_stream(client_stream, RpcClientOptions::default())))
    }

    #[test]
    fn session_state_reads_every_field_the_proxy_mirrors() {
        let state = RemoteSessionState::from_rpc(&state_value());
        assert_eq!(state.thinking_level.as_deref(), Some("high"));
        assert_eq!(state.service_tier.as_deref(), Some("priority"));
        assert_eq!(state.last_abort_source.as_deref(), Some("user"));
        assert!(state.thinking_selection.is_some());
        assert!(state.fast_mode);
        assert!(state.is_bash_running);
        assert_eq!(state.retry_attempt, 2.);
        assert_eq!(state.session_id.as_deref(), Some("one"));
        assert_eq!(state.session_file.as_deref(), Some("/sessions/one.jsonl"));
        assert_eq!(state.steering_mode.as_deref(), Some("all"));
        assert_eq!(state.follow_up_mode.as_deref(), Some("one-at-a-time"));
        assert!(state.project_trusted);
        assert!(state.auto_compaction_enabled);
        assert_eq!(state.steering, ["a"]);
        assert_eq!(state.follow_up, ["b"]);
        assert_eq!(state.pending_message_count, 2.);
        assert_eq!(state.message_count, 4.);
        assert_eq!(state.next_queued_input_order(), 3.);
        assert_eq!(
            state.entries.as_ref().and_then(|entries| entries.get(0)).and_then(|entry| entry.get("id")).and_then(Value::as_str),
            Some("e1")
        );
        assert!(state.usage_totals.is_some());
        assert!(state.context_usage.is_some());
    }

    #[test]
    fn session_state_defaults_missing_keys_without_panicking() {
        let state = RemoteSessionState::from_rpc(&json!({"thinkingLevel": "off", "sessionId": "s"}));
        assert_eq!(state.thinking_level.as_deref(), Some("off"));
        assert!(!state.is_streaming);
        assert!(!state.is_bash_running);
        assert_eq!(state.next_queued_input_order(), 0.);
        assert!(state.model.is_none());
        assert!(state.entries.is_none());
        assert!(state.usage_totals.is_none());
    }

    #[test]
    fn hydrate_applies_text_deltas_and_tracks_usage() {
        let mut streaming = json!({"role": "assistant", "content": [{"type": "text", "text": "he"}]});
        let event = json!({"type": "message_update", "usage": {"input": 5}, "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": "llo"}});
        let hydrated = hydrate_message_update(&event, Some(&mut streaming));
        assert_eq!(hydrated["message"]["content"][0]["text"], "hello");
        assert_eq!(hydrated["message"]["usage"]["input"], 5);
        assert_eq!(hydrated["assistantMessageEvent"]["partial"]["content"][0]["text"], "hello");
    }

    #[test]
    fn hydrate_parses_toolcall_arguments_from_each_delta() {
        let mut streaming = json!({"content": [{"type": "toolCall", "id": "c", "name": "read", "arguments": {}}]});
        hydrate_message_update(
            &json!({"type": "message_update", "assistantMessageEvent": {"type": "toolcall_delta", "contentIndex": 0, "delta": "{\"path\":\"a\"}"}}),
            Some(&mut streaming),
        );
        assert_eq!(streaming["content"][0]["arguments"]["path"], "a");
    }

    #[test]
    fn hydrate_leaves_non_delta_and_unknown_events_untouched() {
        let mut streaming = json!({"content": [{"type": "text", "text": "x"}]});
        let event = json!({"type": "agent_start"});
        assert_eq!(hydrate_message_update(&event, Some(&mut streaming)), event);
        let unknown = json!({"type": "message_update", "assistantMessageEvent": {"type": "start"}});
        assert_eq!(hydrate_message_update(&unknown, Some(&mut streaming)), unknown);
        let none = json!({"type": "message_update"});
        assert_eq!(hydrate_message_update(&none, None), none);
    }

    #[test]
    fn reconcile_entries_only_rebuilds_on_a_mismatch_and_keeps_late_arrivals() {
        let authoritative = vec![json!({"id": "e1"}), json!({"id": "e2"})];
        assert!(reconcile_entries(&authoritative, &authoritative.clone(), &Default::default()).is_none(), "a matching mirror needs no rebuild");
        let mirror = vec![json!({"id": "e1"})];
        let rebuilt = reconcile_entries(&authoritative, &mirror, &Default::default()).expect("rebuild");
        assert_eq!(rebuilt.len(), 2);
        let late = json!({"id": "late"});
        let start: std::collections::BTreeSet<String> = ["e1".to_owned()].into_iter().collect();
        let rebuilt = reconcile_entries(&authoritative, &[json!({"id": "e1"}), late.clone()], &start).expect("rebuild");
        assert_eq!(rebuilt.last(), Some(&late), "an entry that crossed the refresh is kept");
        assert!(reconcile_entries(&[], &mirror, &Default::default()).is_none(), "an empty authoritative list never rebuilds");
    }

    #[test]
    fn host_event_classification_matches_the_client_event_union() {
        assert!(is_interactive_host_event(&json!({"type": "agent_start"})));
        assert!(!is_interactive_host_event(&json!({"no": "type"})));
        assert!(!is_interactive_host_event(&json!([1, 2])));
        assert!(is_connection_level_event(&json!({"type": "account_failover"})));
        assert!(!is_connection_level_event(&json!({"type": "agent_start"})));
    }

    #[test]
    fn warnings_and_capabilities_match_the_pinned_strings() {
        assert_eq!(INTERACTIVE_HOST_FALLBACK_WARNING, "Warning: shared interactive host unavailable; continuing locally");
        assert_eq!(INTERACTIVE_HOST_RECONNECTING_WARNING, "Warning: shared interactive host connection lost; reconnecting");
        assert_eq!(HOST_CLIENT_CAPABILITIES, ["rendered_components", "question"]);
        assert_eq!(client_capabilities(), vec!["rendered_components".to_owned(), "question".to_owned()]);
    }

    #[test]
    fn queue_snapshot_reports_the_ordered_inputs() {
        let snapshot = queue_snapshot(&RemoteSessionState::from_rpc(&state_value()));
        assert_eq!(snapshot["steering"], json!(["a"]));
        assert_eq!(snapshot["pendingMessageCount"], json!(2.));
    }

    #[tokio::test]
    async fn queue_updates_and_reservation_track_the_enqueue_order() {
        let proxy = proxy();
        proxy.apply_state_event(&json!({"type": "queue_update", "steering": ["s"], "followUp": ["f"], "ordered": [{"text": "s", "mode": "steer", "enqueueOrder": 7}]}));
        assert_eq!(proxy.state().steering, ["s"]);
        assert_eq!(proxy.state().pending_message_count, 2.);
        assert_eq!(proxy.reserve_queued_input_order(), 8.);
        assert_eq!(proxy.reserve_queued_input_order(), 9.);
        proxy.apply_state_event(&json!({"type": "agent_settled"}));
        assert!(!proxy.state().is_streaming);
    }

    #[tokio::test]
    async fn stream_state_events_mirror_start_compaction_retry_and_model() {
        let proxy = proxy();
        proxy.apply_state_event(&json!({"type": "agent_start"}));
        assert!(proxy.state().is_streaming);
        proxy.apply_state_event(&json!({"type": "compaction_start"}));
        assert!(proxy.state().is_compacting);
        proxy.apply_state_event(&json!({"type": "compaction_end"}));
        assert!(!proxy.state().is_compacting);
        proxy.apply_state_event(&json!({"type": "auto_retry_start", "attempt": 3}));
        assert_eq!(proxy.state().retry_attempt, 3.);
        proxy.apply_state_event(&json!({"type": "model_changed", "model": {"provider": "x", "id": "y"}, "thinkingLevel": "low"}));
        assert_eq!(proxy.state().model.as_ref().and_then(|model| model.get("id")).and_then(Value::as_str), Some("y"));
        assert_eq!(proxy.state().thinking_level.as_deref(), Some("low"));
        proxy.apply_state_event(&json!({"type": "session_settings_changed", "steeringMode": "all", "followUpMode": "all", "autoCompactionEnabled": true}));
        assert_eq!(proxy.state().steering_mode.as_deref(), Some("all"));
        assert!(proxy.state().auto_compaction_enabled);
        proxy.apply_state_event(&json!({"type": "session_info_changed", "name": "renamed"}));
        assert_eq!(proxy.state().session_name.as_deref(), Some("renamed"));
    }

    #[tokio::test]
    async fn wire_events_hydrate_for_subscribers_and_hold_ui_requests_until_a_handler_attaches() {
        let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured = seen.clone();
        let proxy = proxy();
        let _subscription = proxy.subscribe(Arc::new(move |event| captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(event.clone())));
        proxy.handle_wire_event(&json!({"type": "message_start", "message": {"role": "assistant", "content": [{"type": "text", "text": ""}]}}));
        proxy.handle_wire_event(&json!({"type": "message_update", "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": "hi"}}));
        let events = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let update = events.iter().find(|event| event["type"] == "message_update").expect("update");
        assert_eq!(update["message"]["content"][0]["text"], "hi");
        drop(events);
        proxy.handle_wire_event(&json!({"type": "extension_ui_request", "id": "ui", "request": "select"}));
        assert!(proxy.take_ui_responses().is_empty(), "no handler yet, the request is held");
        let answered = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured = answered.clone();
        proxy.set_host_ui_handler(Some(Arc::new(move |request| {
            captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(request.clone());
            Some(json!({"type": "extension_ui_response", "id": request["id"]}))
        })));
        assert_eq!(answered.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len(), 1, "the held request is replayed to the new handler");
        let responses = proxy.take_ui_responses();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["id"], "ui");
        assert!(proxy.take_ui_responses().is_empty(), "responses are drained once");
    }

    #[tokio::test]
    async fn subscriptions_stop_delivering_after_they_are_dropped() {
        let count = Arc::new(Mutex::new(0usize));
        let captured = count.clone();
        let proxy = proxy();
        let subscription = proxy.subscribe(Arc::new(move |_| *captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner) += 1));
        proxy.handle_wire_event(&json!({"type": "agent_start"}));
        assert_eq!(*count.lock().unwrap_or_else(std::sync::PoisonError::into_inner), 1);
        drop(subscription);
        proxy.handle_wire_event(&json!({"type": "agent_settled"}));
        assert_eq!(*count.lock().unwrap_or_else(std::sync::PoisonError::into_inner), 1, "a dropped subscription stops delivery");
    }

    #[tokio::test]
    async fn decoded_session_events_reach_session_listeners_and_ignore_unknown_records() {
        let received = Arc::new(Mutex::new(Vec::<maho_ext_api::AgentSessionEvent>::new()));
        let captured = received.clone();
        let proxy = proxy();
        let subscription = proxy.subscribe_session_events(Arc::new(move |event| captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(event.clone())));
        proxy.handle_wire_event(&json!({"type": "continuation_error", "errorMessage": "bridge"}));
        proxy.handle_wire_event(&json!({"type": "not_a_real_event"}));
        let events = received.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        assert_eq!(events.len(), 1, "an unknown record is not delivered");
        assert!(matches!(&events[0], maho_ext_api::AgentSessionEvent::ContinuationError { error_message } if error_message.as_str() == "bridge"));
        drop(subscription);
        proxy.handle_wire_event(&json!({"type": "agent_idle"}));
        assert_eq!(received.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len(), 1, "a dropped session subscription stops delivery");
    }

    #[tokio::test]
    async fn action_failures_surface_only_for_command_refusals_not_transport_loss() {
        let seen = Arc::new(Mutex::new(Vec::<InteractiveHostWarning>::new()));
        let captured = seen.clone();
        let proxy = RemoteSessionProxy::new(
            RemoteSessionState::default(),
            detached_client(),
            Some(Arc::new(move |warning| captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(warning))),
        );
        proxy.report_action_failure("setModel", &RpcClientError::Transport(std::io::Error::other("gone")));
        assert!(seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty(), "transport loss is the event stream's business");
        proxy.report_action_failure("setModel", &RpcClientError::Command(maho_rpc::rpc_client::RpcCommandError { message: "refused".into(), error_code: None, error_data: None }));
        let warnings = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, InteractiveHostWarningKind::InteractiveHostActionFailed);
        assert!(warnings[0].message.contains("setModel failed: refused"));
    }

    #[test]
    fn fallback_without_a_session_file_never_starts_a_host() {
        match block_on(create_interactive_host_runtime(binding(None), InteractiveHostRuntimeOptions::default())) {
            InteractiveHostRuntimeOutcome::Fallback(warning) => {
                assert_eq!(warning.kind, InteractiveHostWarningKind::InteractiveHostFallback);
                assert_eq!(warning.message, INTERACTIVE_HOST_FALLBACK_WARNING);
                assert_eq!(warning.cause, "session has no file on disk");
            }
            InteractiveHostRuntimeOutcome::Remote(_) => panic!("an in-memory session has no host"),
        }
    }

    #[test]
    fn ensure_host_override_reports_its_refusal_as_the_fallback_cause() {
        let options = InteractiveHostRuntimeOptions {
            socket: "/tmp/agent/rpc/rpc.sock".into(),
            ensure_host: Some(Arc::new(|_| Err("host_busy".into()))),
            ..Default::default()
        };
        match block_on(create_interactive_host_runtime(binding(Some("/sessions/one.jsonl")), options)) {
            InteractiveHostRuntimeOutcome::Fallback(warning) => assert_eq!(warning.cause, "host_busy"),
            InteractiveHostRuntimeOutcome::Remote(_) => panic!("the injected ensure refused"),
        }
    }

    #[tokio::test]
    async fn runtime_lifecycle_is_monotonic_into_fallback_and_disposed() {
        let runtime = detached_runtime();
        assert_eq!(runtime.state(), HostRuntimeState::Connected);
        runtime.enter_reconnecting();
        assert!(runtime.is_reconnecting());
        runtime.enter_connected();
        assert!(!runtime.is_reconnecting());
        runtime.enter_fallback("closed".into()).await;
        assert!(runtime.is_fallback());
        assert!(!runtime.reconnect().await, "a fallen-back runtime does not reconnect");
        assert_eq!(runtime.state(), HostRuntimeState::Fallback);
        runtime.dispose().await;
        assert_eq!(runtime.state(), HostRuntimeState::Disposed);
        runtime.dispose().await;
        assert_eq!(runtime.state(), HostRuntimeState::Disposed);
        runtime.enter_connected();
        assert_eq!(runtime.state(), HostRuntimeState::Disposed, "a disposed runtime never reconnects");
    }

    #[tokio::test]
    async fn fallback_invokes_the_invalidate_and_rebind_hooks_in_order() {
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let runtime = detached_runtime();
        let invalidate = order.clone();
        runtime.set_before_session_invalidate(Some(Box::new(move || invalidate.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push("invalidate"))));
        let rebind = order.clone();
        runtime.set_rebind_session(Some(Arc::new(move || {
            let rebind = rebind.clone();
            Box::pin(async move {
                rebind.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push("rebind");
                Ok(())
            })
        })));
        runtime.enter_fallback("transport gone".into()).await;
        assert_eq!(*order.lock().unwrap_or_else(std::sync::PoisonError::into_inner), ["invalidate", "rebind"]);
    }

    #[tokio::test]
    async fn replacement_calls_short_circuit_once_fallen_back() {
        let runtime = detached_runtime();
        runtime.enter_fallback("gone".into()).await;
        assert!(runtime.new_session(None).await.expect("local handoff"), "a fallen-back runtime reports the local handoff");
        assert!(runtime.switch_session("/sessions/two.jsonl", None).await.expect("local handoff"));
        assert!(runtime.fork("e1", None).await.expect("local handoff"));
        assert!(runtime.import_jsonl("/tmp/import.jsonl", None).await.expect("local handoff"));
    }

    #[tokio::test]
    async fn client_info_sends_capabilities_once_then_width_only() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let (client_stream, host) = tokio::net::UnixStream::pair().expect("socket pair");
        let client = Arc::new(tokio::sync::Mutex::new(RpcClient::from_stream(client_stream, RpcClientOptions::default())));
        let proxy = Arc::new(RemoteSessionProxy::new(RemoteSessionState::default(), client.clone(), None));
        let runtime = RemoteInteractiveRuntime::new(client, proxy, binding(None), InteractiveHostRuntimeOptions::default());
        let reader = tokio::spawn(async move {
            let (read, _write) = host.into_split();
            let mut lines = BufReader::new(read).lines();
            let mut frames = Vec::new();
            for _ in 0..2 {
                if let Some(line) = lines.next_line().await.expect("read frame") {
                    frames.push(serde_json::from_str::<Value>(&line).expect("frame json"));
                }
            }
            frames
        });
        runtime.set_client_info(120.).await;
        runtime.set_client_info(121.).await;
        let frames = reader.await.expect("reader");
        assert_eq!(frames[0]["type"], "set_client_info");
        assert_eq!(frames[0]["width"], json!(120.));
        assert_eq!(frames[0]["capabilities"], json!(["rendered_components", "question"]), "capabilities ride the first frame");
        assert_eq!(frames[1]["width"], json!(121.));
        assert!(frames[1].get("capabilities").is_none(), "later frames carry the width only");
    }

    #[tokio::test]
    async fn proxy_fire_and_forget_commands_forward_frames_and_mirror_queued_input() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let (client_stream, host) = tokio::net::UnixStream::pair().expect("socket pair");
        let client = Arc::new(tokio::sync::Mutex::new(RpcClient::from_stream(client_stream, RpcClientOptions::default())));
        let proxy = RemoteSessionProxy::new(RemoteSessionState::default(), client, None);
        let reader = tokio::spawn(async move {
            let (read, _write) = host.into_split();
            let mut lines = BufReader::new(read).lines();
            let mut frames = Vec::new();
            for _ in 0..5 {
                if let Some(line) = lines.next_line().await.expect("read frame") {
                    frames.push(serde_json::from_str::<Value>(&line).expect("frame json"));
                }
            }
            frames
        });
        proxy.steer("queued", None, None).await.expect("steer");
        proxy.follow_up("later", None, None).await.expect("follow up");
        proxy.set_auto_compaction(true).await.expect("auto compaction");
        proxy.set_steering_mode("all").await.expect("steering mode");
        proxy.set_favorite_models(json!([{"provider": "anthropic", "id": "claude"}])).await.expect("favorites");
        let frames = reader.await.expect("reader");
        let types = frames.iter().filter_map(|frame| frame["type"].as_str()).collect::<Vec<_>>();
        assert_eq!(types, ["steer", "follow_up", "set_auto_compaction", "set_steering_mode", "set_favorite_models"]);
        assert_eq!(frames[0]["message"], "queued");
        assert_eq!(frames[0]["enqueueOrder"], json!(1.), "the first queued input takes the next order");
        assert_eq!(frames[1]["enqueueOrder"], json!(2.));
        assert_eq!(proxy.steering_messages(), ["queued"]);
        assert_eq!(proxy.follow_up_messages(), ["later"]);
        assert_eq!(proxy.pending_message_count(), 2.);
        assert_eq!(proxy.favorite_models(), json!([{"provider": "anthropic", "id": "claude"}]));
    }

    #[tokio::test]
    async fn proxy_request_commands_return_the_host_data_and_swallow_transport_loss() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (client_stream, host) = tokio::net::UnixStream::pair().expect("socket pair");
        let client = Arc::new(tokio::sync::Mutex::new(RpcClient::from_stream(client_stream, RpcClientOptions::default())));
        let proxy = RemoteSessionProxy::new(RemoteSessionState::default(), client, None);
        let responder = tokio::spawn(async move {
            let (read, mut write) = host.into_split();
            let mut lines = BufReader::new(read).lines();
            let mut types = Vec::new();
            for _ in 0..4 {
                let Some(line) = lines.next_line().await.expect("read frame") else { break; };
                let frame: Value = serde_json::from_str(&line).expect("frame json");
                types.push(frame["type"].as_str().unwrap_or_default().to_owned());
                let response = json!({"type": "response", "id": frame["id"], "success": true, "data": {"echo": frame["type"]}});
                write.write_all(format!("{response}\n").as_bytes()).await.expect("write response");
            }
            types
        });
        let model = proxy.set_model("anthropic", "claude").await.expect("set model");
        assert_eq!(model["echo"], "set_model");
        let levels = proxy.get_available_thinking_levels().await.expect("levels");
        assert_eq!(levels["echo"], "get_available_thinking_levels");
        let compacted = proxy.compact(Some("tight")).await.expect("compact");
        assert_eq!(compacted["echo"], "compact");
        let snapshot = proxy.clear_queue(Some(true)).await;
        assert_eq!(snapshot["steering"], json!([]), "clearQueue returns the mirrored queue");
        assert_eq!(snapshot["ordered"], json!([]));
        let types = responder.await.expect("responder");
        assert_eq!(types, ["set_model", "get_available_thinking_levels", "compact", "clear_queue"]);
        // A transport loss yields the cancelled value instead of an error.
        proxy.client.lock().await.stop().await;
        assert_eq!(proxy.compact(None).await.expect("cancelled"), Value::Null);
    }

    fn detached_runtime() -> RemoteInteractiveRuntime {
        let client = detached_client();
        let proxy = Arc::new(RemoteSessionProxy::new(RemoteSessionState::default(), client.clone(), None));
        RemoteInteractiveRuntime::new(client, proxy, binding(Some("/sessions/one.jsonl")), InteractiveHostRuntimeOptions::default())
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime").block_on(future)
    }
}