//! Port of senpi mcp/modern/request.ts: the multi-round-trip request (MRTR) engine.
//!
//! Upstream monkey-patches `client.request`; the port routes every MCP request through `mcp_request`, which
//! keeps the plain request until negotiation positively selects modern (or private forms are enforced).
use super::{
    consent_queue::queued_form,
    elicitation::{MrtrInputResponse, collect_mrtr_form},
    errors::{McpMrtrError, McpMrtrErrorCode, as_mcp_mrtr_error},
    form_schema::validate_mrtr_form,
    inputs::parse_inputs,
    tasks::{MCP_TASK_EXTENSION, McpTaskOptions, follow_mcp_task, mcp_task_timeout_ms},
};
use crate::{
    connection::ServerConnectionState,
    errors::{McpError, McpErrorKind},
    form_types::{McpFormInput, McpFormOutcomeKind, McpFormServer, McpFormServerSource},
    request_origin::{
        McpRequestOptions, McpRequestOrigin, begin_legacy_mcp_operation, take_mcp_request_origin,
    },
    transport_sdk::McpClient,
    wrap::{
        MCP_ELAPSED_CLOCK, McpClock, McpHostSignal, McpLinkedSignal, mcp_interrupted,
        on_mcp_client_close,
    },
};
use futures::FutureExt;
use maho_ext_api::AbortSignal;
use serde_json::{Map, Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub const MAX_MCP_MRTR_ROUNDS: u32 = 8;
const CAPABLE_METHODS: [&str; 3] = ["tools/call", "resources/read", "prompts/get"];
const CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
/// Upstream SDK `DEFAULT_REQUEST_TIMEOUT_MSEC`.
pub const DEFAULT_REQUEST_TIMEOUT_MS: f64 = 60_000.0;
const CONSENT_BUDGET_MS: f64 = 300_000.0;

/// Local dispatch guard: runs at the network boundary with the credential snapshot about to be sent
/// (upstream `modernDispatchGuard`, a symbol that survives SDK spreads but is never serialized).
pub type DispatchGuard = Arc<dyn Fn(Option<String>) -> Result<(), McpMrtrError> + Send + Sync>;

/// Per-physical-client negotiation state (upstream `ModernProtocolState`, one per SDK client).
#[derive(Default)]
pub struct ModernProtocolState {
    pub private_forms: AtomicBool,
    legacy_form_eligible: Mutex<Option<bool>>,
    version: Mutex<Option<String>>,
    pub legacy: AtomicBool,
    /// Upstream `resolveStdioDiagnostics`: the stdio child's stderr, held until the era is classified.
    pub stdio_diagnostics: Mutex<Option<Arc<crate::transport_sdk::StderrDiagnostics>>>,
}
impl ModernProtocolState {
    pub fn version(&self) -> Option<String> {
        self.version
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    /// Upstream `protocol.version = v; protocol.activateRequests()`.
    pub fn set_version(&self, version: &str) {
        *self
            .version
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(version.into());
    }
    pub fn legacy_form_eligible(&self) -> Option<bool> {
        *self
            .legacy_form_eligible
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub fn set_legacy_form_eligible(&self, eligible: bool) {
        *self
            .legacy_form_eligible
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(eligible);
    }
    /// Upstream `protocol.resolveStdioDiagnostics(modern)`: release or discard held stdio stderr.
    pub fn resolve_stdio_diagnostics(&self, modern: bool) {
        let diagnostics = self
            .stdio_diagnostics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(diagnostics) = diagnostics {
            diagnostics.resolve(self, modern);
        }
    }
    /// Default legacy clients retain the plain request and its original admission behavior; enforced legacy
    /// calls need private operation ownership before the first tools/call.
    pub fn requests_active(&self) -> bool {
        self.private_forms.load(Ordering::Acquire) || self.version().is_some()
    }
}
pub fn is_modern_mcp_client(client: &McpClient) -> bool {
    client.protocol.version().is_some()
}
pub fn legacy_mcp_forms_advertised(client: &McpClient) -> Option<bool> {
    client.protocol.legacy_form_eligible()
}

enum Fail {
    Mrtr(McpMrtrError),
    Other(McpError),
}
impl From<McpMrtrError> for Fail {
    fn from(error: McpMrtrError) -> Self {
        Self::Mrtr(error)
    }
}
impl From<McpError> for Fail {
    fn from(error: McpError) -> Self {
        match as_mcp_mrtr_error(&error) {
            Some(mrtr) => Self::Mrtr(mrtr),
            None => Self::Other(error),
        }
    }
}
fn code(code: McpMrtrErrorCode) -> Fail {
    Fail::Mrtr(McpMrtrError::new(code))
}

struct OperationState {
    failure: Mutex<Option<McpMrtrError>>,
    signal: AbortSignal,
    deadline: Mutex<f64>,
    presentation_lost: AtomicBool,
}
impl OperationState {
    fn stop(&self, code: McpMrtrErrorCode) {
        self.failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert_with(|| McpMrtrError::new(code));
        self.signal.abort();
    }
    fn failure(&self) -> Option<McpMrtrError> {
        self.failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

struct Operation {
    client: Arc<McpClient>,
    origin: Option<Arc<McpRequestOrigin>>,
    state: Arc<OperationState>,
    clock: Arc<dyn McpClock>,
    start: f64,
    generation: Option<u64>,
    credential: Option<String>,
    tasks_enabled: bool,
    authority: Mutex<Option<String>>,
    consent_spent: Mutex<f64>,
    on_progress: Option<Arc<dyn Fn(Value) + Send + Sync>>,
}

fn origin_changed(origin: &McpRequestOrigin, generation: Option<u64>) -> bool {
    !origin.current()
        || origin.signal.is_aborted()
        || Some(origin.connection.generation()) != generation
        || origin.connection.state() != ServerConnectionState::Connected
}

impl Operation {
    fn check(&self) -> Result<(), McpMrtrError> {
        if self.clock.now()
            >= *self
                .state
                .deadline
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            self.state.stop(McpMrtrErrorCode::Timeout);
        }
        if let Some(origin) = &self.origin
            && origin_changed(origin, self.generation)
        {
            self.state.stop(McpMrtrErrorCode::ContextChanged);
        }
        self.state.failure().map_or(Ok(()), Err)
    }
    async fn resolve_authority(&self) -> Result<(), Fail> {
        self.check()?;
        if let Some(origin) = &self.origin {
            if let Some(form) = &origin.private_form
                && let Some(synchronize) = &form.synchronize
            {
                synchronize().await;
            }
            let resolved = mcp_interrupted(
                (origin.resolve_authority)(self.state.signal.clone()),
                &self.state.signal,
            )
            .await
            .map_err(|_| {
                self.check()
                    .err()
                    .unwrap_or_else(|| McpMrtrError::new(McpMrtrErrorCode::Cancelled))
            })??;
            let snapshot = resolved.to_string();
            let mut authority = self
                .authority
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match &*authority {
                None => *authority = Some(snapshot),
                Some(previous) if *previous != snapshot => {
                    self.state.stop(McpMrtrErrorCode::ContextChanged)
                }
                Some(_) => {}
            }
        }
        self.check()?;
        Ok(())
    }
    fn request_capabilities(&self) -> Value {
        let mut capabilities = Map::new();
        let forms = match &self.origin {
            Some(origin) if origin.private_forms_enforced => origin
                .private_form
                .as_ref()
                .is_some_and(|form| (form.ready)()),
            Some(origin) => {
                // Upstream's abort listener runs synchronously; read the signal too, not only the watcher's flag.
                !self.state.presentation_lost.load(Ordering::Acquire)
                    && origin
                        .presentation
                        .as_ref()
                        .is_some_and(|presentation| !presentation.signal.aborted())
            }
            None => false,
        };
        if forms {
            capabilities.insert("elicitation".into(), json!({}));
        }
        if self.tasks_enabled {
            capabilities.insert("extensions".into(), json!({ MCP_TASK_EXTENSION: {} }));
        }
        Value::Object(capabilities)
    }
    fn guard(self: &Arc<Self>, with_check: bool) -> DispatchGuard {
        let operation = self.clone();
        Arc::new(move |snapshot: Option<String>| {
            if with_check {
                operation.check()?;
            }
            let current = snapshot.or_else(|| operation.client.credential_snapshot());
            if current != operation.credential {
                return Err(McpMrtrError::new(McpMrtrErrorCode::ContextChanged));
            }
            Ok(())
        })
    }
    async fn collect_inputs(
        &self,
        result: &Map<String, Value>,
        active: &AbortSignal,
        round: u32,
    ) -> Result<Map<String, Value>, Fail> {
        let inputs = parse_inputs(result)?;
        let mut responses = Map::new();
        let mut cancelled = false;
        for (key, input) in inputs.unwrap_or_default() {
            self.resolve_authority().await?;
            if active.is_aborted() {
                return Err(code(McpMrtrErrorCode::Cancelled));
            }
            if cancelled {
                return Err(code(McpMrtrErrorCode::Cancelled));
            }
            let spent = *self
                .consent_spent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if spent >= CONSENT_BUDGET_MS {
                return Err(code(McpMrtrErrorCode::ProcessingUnavailable));
            }
            let origin = self.origin.as_ref();
            let answer = if let Some(origin) = origin.filter(|origin| origin.private_forms_enforced)
            {
                let Some(presenter) = origin.private_form.as_ref().filter(|form| (form.ready)())
                else {
                    return Err(code(McpMrtrErrorCode::ProcessingUnavailable));
                };
                let Some(schema) = input
                    .requested_schema
                    .as_object()
                    .filter(|_| validate_mrtr_form(&input.requested_schema))
                else {
                    return Err(code(McpMrtrErrorCode::ProcessingUnavailable));
                };
                let began = self.clock.now();
                let operation = McpHostSignal::follow(active);
                let outcome = (presenter.present)(McpFormInput {
                    operation_id: maho_ai::utils::js::number_to_string(self.start),
                    input_id: key.clone(),
                    round,
                    server: McpFormServer {
                        label: origin.server_label.clone(),
                        source: origin.server_source.unwrap_or(McpFormServerSource::User),
                    },
                    message: input.message.clone(),
                    schema: Value::Object(schema.clone()),
                    operation_signal: operation.signal.clone(),
                    operation_current: None,
                    presentation_signal: presenter.signal.clone(),
                    deadline_ms: (CONSENT_BUDGET_MS - spent).max(0.0),
                })
                .await;
                drop(operation);
                *self
                    .consent_spent
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) += self.clock.now() - began;
                self.check()?;
                match outcome.kind {
                    // The presenter answers with a validated `MrtrInputResponse` as JSON.
                    McpFormOutcomeKind::Result(result) => {
                        serde_json::from_value::<MrtrInputResponse>(result)
                            .map_err(|_| code(McpMrtrErrorCode::ProcessingUnavailable))?
                    }
                    McpFormOutcomeKind::OperationAborted => {
                        return Err(code(McpMrtrErrorCode::Cancelled));
                    }
                    McpFormOutcomeKind::ProcessingFailure => {
                        return Err(code(McpMrtrErrorCode::ProcessingUnavailable));
                    }
                }
            } else {
                let presentation = origin.and_then(|origin| origin.presentation.clone());
                let (Some(origin), Some(presentation)) = (origin, presentation) else {
                    return Err(code(McpMrtrErrorCode::ProcessingUnavailable));
                };
                if self.state.presentation_lost.load(Ordering::Acquire)
                    || presentation.signal.aborted()
                {
                    return Err(code(McpMrtrErrorCode::ProcessingUnavailable));
                }
                let consent = AbortSignal::default();
                let began = self.clock.now();
                let consent_timer = {
                    let consent = consent.clone();
                    let sleep = self.clock.sleep((CONSENT_BUDGET_MS - spent).max(0.0));
                    tokio::spawn(async move {
                        sleep.await;
                        consent.abort();
                    })
                };
                let linked = McpLinkedSignal::with_host(
                    vec![active.clone(), consent.clone()],
                    vec![presentation.signal.clone()],
                );
                let presented = AtomicBool::new(false);
                let answer = mcp_interrupted(
                    queued_form(origin.binding, &linked.signal, || {
                        presented.store(true, Ordering::Release);
                        collect_mrtr_form(
                            &presentation,
                            &origin.server_label,
                            &input.message,
                            &input.requested_schema,
                            &linked.signal,
                        )
                    }),
                    active,
                )
                .await;
                *self
                    .consent_spent
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) += self.clock.now() - began;
                consent_timer.abort();
                consent.abort();
                let answer = answer.map_err(|_| {
                    self.check()
                        .err()
                        .unwrap_or_else(|| McpMrtrError::new(McpMrtrErrorCode::Cancelled))
                })??;
                self.check()?;
                if !presented.load(Ordering::Acquire) {
                    return Err(code(McpMrtrErrorCode::ProcessingUnavailable));
                }
                answer
            };
            cancelled = !answer.accepted();
            responses.insert(key, serde_json::to_value(&answer).unwrap_or(Value::Null));
        }
        if serde_json::to_string(&responses).map_or(0, |text| text.len()) > 512 * 1024 {
            return Err(code(McpMrtrErrorCode::InputLimit));
        }
        Ok(responses)
    }
}

fn remote_code(error: &McpError) -> Option<i64> {
    error
        .cause
        .as_deref()
        .and_then(|cause| cause.get("code"))
        .and_then(Value::as_i64)
}
fn is_parameter_header_error(error: &McpError) -> bool {
    error
        .cause
        .as_deref()
        .and_then(|cause| cause.get("name"))
        .and_then(Value::as_str)
        == Some("ParameterHeaderValidationError")
}

/// Every MCP request: legacy requests pass through untouched; modern ones run the MRTR loop.
pub async fn mcp_request(
    client: &Arc<McpClient>,
    method: &str,
    params: Value,
    timeout: Duration,
    options: &McpRequestOptions,
) -> Result<Value, McpError> {
    mcp_request_with_clock(
        client,
        method,
        params,
        timeout,
        options,
        Arc::new(MCP_ELAPSED_CLOCK),
    )
    .await
}

pub async fn mcp_request_with_clock(
    client: &Arc<McpClient>,
    method: &str,
    params: Value,
    timeout: Duration,
    options: &McpRequestOptions,
    clock: Arc<dyn McpClock>,
) -> Result<Value, McpError> {
    if method == "initialize" || !client.protocol.requests_active() {
        return client
            .request_guarded(method, params, timeout, options.signal.as_ref(), None)
            .await;
    }
    let origin = take_mcp_request_origin(options, client)?;
    if client.protocol.version().is_none() {
        let _operation = (method == "tools/call")
            .then_some(origin.clone())
            .flatten()
            .map(|origin| begin_legacy_mcp_operation(client, origin, options.signal.as_ref()));
        return client
            .request_guarded(method, params, timeout, options.signal.as_ref(), None)
            .await;
    }
    let mut params = match params {
        Value::Object(params) => params,
        Value::Null => Map::new(),
        _ => return Err(McpMrtrError::new(McpMrtrErrorCode::InvalidInput).into()),
    };
    if params.contains_key("requestState") || params.contains_key("inputResponses") {
        return Err(McpMrtrError::new(McpMrtrErrorCode::InvalidInput).into());
    }
    let start = clock.now();
    let timeout_ms = timeout.as_secs_f64() * 1000.0;
    let total = options
        .max_total_timeout
        .map_or(f64::INFINITY, |total| total.as_secs_f64() * 1000.0);
    let duration = total.min(9.0 * timeout_ms + 300_000.0);
    let tasks_enabled = client
        .server_capabilities
        .read()
        .await
        .get("extensions")
        .and_then(|extensions| extensions.get(MCP_TASK_EXTENSION))
        .is_some_and(Value::is_object);
    let task_timeout = if tasks_enabled && method == "tools/call" {
        mcp_task_timeout_ms()
            .map_err(|error| McpError::new(McpErrorKind::ToolExec, error.to_string()))?
    } else {
        0.0
    };
    let credential = client.fresh_credential_snapshot().await;
    let state = Arc::new(OperationState {
        failure: Mutex::new(None),
        signal: AbortSignal::default(),
        deadline: Mutex::new(start + duration),
        presentation_lost: AtomicBool::new(origin.as_ref().is_none_or(|origin| {
            origin
                .presentation
                .as_ref()
                .is_none_or(|presentation| presentation.signal.aborted())
        })),
    });
    let operation = Arc::new(Operation {
        client: client.clone(),
        generation: origin.as_ref().map(|origin| origin.connection.generation()),
        origin: origin.clone(),
        state: state.clone(),
        clock: clock.clone(),
        start,
        credential,
        tasks_enabled,
        authority: Mutex::new(None),
        consent_spent: Mutex::new(0.0),
        on_progress: options.on_progress.clone(),
    });
    // Watchers: caller abort cancels, owner abort or a connection transition changes context, the operation
    // deadline times out, and a lost presentation only withdraws the form capability.
    let mut watchers: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    struct Watchers(Vec<tokio::task::JoinHandle<()>>);
    impl Drop for Watchers {
        fn drop(&mut self) {
            for watcher in &self.0 {
                watcher.abort();
            }
        }
    }
    if let Some(signal) = options.signal.clone() {
        let state = state.clone();
        watchers.push(tokio::spawn(async move {
            signal.cancelled().await;
            state.stop(McpMrtrErrorCode::Cancelled);
        }));
    }
    if let Some(origin) = &origin {
        let (signal, owner) = (origin.signal.clone(), state.clone());
        watchers.push(tokio::spawn(async move {
            signal.cancelled().await;
            owner.stop(McpMrtrErrorCode::ContextChanged);
        }));
        if let Some(presentation) = &origin.presentation {
            let (signal, lost) = (presentation.signal.clone(), state.clone());
            watchers.push(tokio::spawn(async move {
                signal.cancelled().await;
                lost.presentation_lost.store(true, Ordering::Release);
            }));
        }
        let mut changes = origin.connection.on_state_change();
        let watched = operation.clone();
        watchers.push(tokio::spawn(async move {
            while let Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) =
                changes.recv().await
            {
                // Abort reaches the active attempt and consent.
                let _ = watched.check();
            }
        }));
    }
    let deadline_timer = {
        let (sleep, state) = (clock.sleep(duration.max(0.0)), state.clone());
        tokio::spawn(async move {
            sleep.await;
            state.stop(McpMrtrErrorCode::Timeout);
        })
    };
    let mut watchers = Watchers(watchers);
    // Aborted on every exit, including when the caller drops this request mid-flight.
    let mut deadline_timer = Watchers(vec![deadline_timer]);
    let mut task_close = None;
    let metadata = params
        .remove("_meta")
        .and_then(|meta| meta.as_object().cloned())
        .unwrap_or_default();
    let mut continuation = Map::new();
    let result: Result<Value, Fail> = async {
        let mut round = 0u32;
        loop {
            operation.resolve_authority().await?;
            let remaining = *state.deadline.lock().unwrap_or_else(std::sync::PoisonError::into_inner) - clock.now();
            let attempt = McpLinkedSignal::new(vec![state.signal.clone()]);
            let mut outgoing = params.clone();
            outgoing.extend(continuation.clone());
            let mut meta = metadata.clone();
            meta.insert(CAPABILITIES_KEY.into(), operation.request_capabilities());
            // Upstream wraps `onprogress` per attempt; the SDK overwrites `_meta.progressToken` with its own key.
            let progress_token = operation.on_progress.as_ref().map(|_| {
                let token = Value::from(format!("mrtr:{}", NEXT_PROGRESS_TOKEN.fetch_add(1, Ordering::Relaxed)));
                meta.insert("progressToken".into(), token.clone());
                (token, client.notifications.subscribe())
            });
            outgoing.insert("_meta".into(), Value::Object(meta));
            operation.check()?;
            let attempt_timeout = Duration::from_secs_f64((timeout_ms.min(remaining).max(0.0)) / 1000.0);
            let request = client.request_guarded(method, Value::Object(outgoing), attempt_timeout, Some(&attempt.signal), Some(operation.guard(true)));
            let response = match (progress_token, operation.on_progress.as_deref()) {
                (Some((token, updates)), Some(on_progress)) => forward_progress(request, updates, &token, &operation, on_progress).await,
                _ => request.await,
            }?;
            drop(attempt);
            let Value::Object(mut result) = response else {
                return Err(code(McpMrtrErrorCode::InvalidResult));
            };
            let mut kind = result.get("resultType").cloned().unwrap_or_else(|| "complete".into());
            if kind == "task" {
                if !tasks_enabled || method != "tools/call" {
                    return Err(code(McpMrtrErrorCode::InvalidResult));
                }
                // The ordinary MRTR deadline ends at the handle, not at the final task result.
                deadline_timer.0.drain(..).for_each(|timer| timer.abort());
                *state.deadline.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = f64::INFINITY;
                let closing = state.clone();
                task_close = Some(on_mcp_client_close(client, move || closing.stop(McpMrtrErrorCode::ContextChanged)));
                let send = |task_method: &'static str, task_params: Value, signal: Option<AbortSignal>| {
                    let operation = operation.clone();
                    let metadata = metadata.clone();
                    async move {
                        if let Some(signal) = &signal {
                            mcp_interrupted(operation.resolve_authority(), signal)
                                .await
                                .map_err(|_| McpMrtrError::new(McpMrtrErrorCode::Cancelled))?
                                .map_err(|fail| match fail {
                                    Fail::Mrtr(error) => error,
                                    Fail::Other(_) => McpMrtrError::new(McpMrtrErrorCode::TransportFailed),
                                })?;
                        }
                        // Cancellation has its own request lifetime, independent of the aborted tool call.
                        let checked = signal.is_some();
                        // Upstream rejects the follower with the abort reason, so a CONTEXT_CHANGED stop never sends
                        // `tasks/cancel`; the tool signal carries no reason, so the stop is read from the operation.
                        if !checked && operation.state.failure().is_some_and(|failure| failure.code == McpMrtrErrorCode::ContextChanged) {
                            return Err(McpMrtrError::new(McpMrtrErrorCode::ContextChanged));
                        }
                        let guard_operation = operation.clone();
                        let guard: DispatchGuard = Arc::new(move |snapshot: Option<String>| {
                            let changed = guard_operation.origin.as_ref().is_some_and(|origin| origin_changed(origin, guard_operation.generation));
                            if changed || snapshot.or_else(|| guard_operation.client.credential_snapshot()) != guard_operation.credential {
                                return Err(McpMrtrError::new(McpMrtrErrorCode::ContextChanged));
                            }
                            if checked {
                                guard_operation.check()?;
                            }
                            Ok(())
                        });
                        guard(None)?;
                        let mut params = task_params.as_object().cloned().unwrap_or_default();
                        let mut meta = metadata.clone();
                        meta.insert(CAPABILITIES_KEY.into(), operation.request_capabilities());
                        params.insert("_meta".into(), Value::Object(meta));
                        let reply = operation
                            .client
                            .request_guarded(task_method, Value::Object(params), timeout, signal.as_ref(), Some(guard))
                            .await
                            .map_err(|error| as_mcp_mrtr_error(&error).unwrap_or_else(|| McpMrtrError::with(McpMrtrErrorCode::TransportFailed, remote_code(&error), None)))?;
                        Ok::<_, McpMrtrError>(reply.as_object().cloned().unwrap_or_default())
                    }
                    .boxed()
                };
                let inputs = |batch: Map<String, Value>, signal: AbortSignal| {
                    let operation = operation.clone();
                    async move {
                        operation.collect_inputs(&batch, &signal, 0).await.map_err(|fail| match fail {
                            Fail::Mrtr(error) => error,
                            Fail::Other(_) => McpMrtrError::new(McpMrtrErrorCode::TransportFailed),
                        })
                    }
                    .boxed()
                };
                let progress = |task_id: &str, status: &str, message: Option<&str>| -> Result<(), McpMrtrError> {
                    operation.check()?;
                    if let Some(on_progress) = &operation.on_progress {
                        let suffix = message.map_or_else(String::new, |message| format!(" - {message}"));
                        on_progress(json!({"progress": 0, "message": format!("MCP task {task_id}: {status}{suffix}")}));
                    }
                    Ok(())
                };
                let task_linked = options.signal.as_ref().map(|caller| {
                    McpLinkedSignal::new(vec![caller.clone(), state.signal.clone()])
                });
                let task_signal = task_linked
                    .as_ref()
                    .map(|linked| linked.signal.clone())
                    .unwrap_or_else(|| state.signal.clone());
                result = follow_mcp_task(
                    &result,
                    McpTaskOptions {
                        clock: clock.as_ref(),
                        signal: task_signal,
                        timeout_ms: task_timeout,
                        send: &send,
                        inputs: &inputs,
                        progress: &progress,
                    },
                )
                .await?;
                kind = result.get("resultType").cloned().unwrap_or_else(|| "complete".into());
                if kind != "complete" {
                    return Err(code(McpMrtrErrorCode::InvalidResult));
                }
            }
            operation.check()?;
            if kind == "complete" {
                if ["requestState", "inputRequests", "inputResponses"].iter().any(|key| result.contains_key(*key)) {
                    return Err(code(McpMrtrErrorCode::InvalidResult));
                }
                return Ok(Value::Object(result));
            }
            if kind != "input_required" || !CAPABLE_METHODS.contains(&method) {
                return Err(code(McpMrtrErrorCode::InvalidResult));
            }
            if round >= MAX_MCP_MRTR_ROUNDS {
                return Err(code(McpMrtrErrorCode::RoundLimit));
            }
            continuation = Map::new();
            if let Some(request_state) = result.get("requestState") {
                continuation.insert("requestState".into(), request_state.clone());
            }
            let responses = operation.collect_inputs(&result, &state.signal, round).await?;
            if result.contains_key("inputRequests") {
                continuation.insert("inputResponses".into(), Value::Object(responses));
            }
            round += 1;
        }
    }
    .await;
    continuation.clear();
    deadline_timer.0.drain(..).for_each(|timer| timer.abort());
    drop(task_close);
    watchers.0.drain(..).for_each(|watcher| watcher.abort());
    match result {
        Ok(value) => Ok(value),
        Err(fail) => {
            if let Err(failure) = operation.check() {
                return Err(failure.into());
            }
            match fail {
                Fail::Mrtr(error) => Err(error.into()),
                Fail::Other(error)
                    if error.kind == McpErrorKind::Auth || is_parameter_header_error(&error) =>
                {
                    Err(error)
                }
                Fail::Other(error) => Err(McpMrtrError::with(
                    McpMrtrErrorCode::TransportFailed,
                    remote_code(&error),
                    None,
                )
                .into()),
            }
        }
    }
}

static NEXT_PROGRESS_TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Upstream attempt `onprogress`: progress for this attempt reaches the caller only while the operation is
/// still live, finite, and stripped to `{progress, total?}`; it never extends the operation deadline.
async fn forward_progress(
    request: impl std::future::Future<Output = Result<Value, McpError>>,
    mut updates: tokio::sync::broadcast::Receiver<Value>,
    token: &Value,
    operation: &Operation,
    on_progress: &(dyn Fn(Value) + Send + Sync),
) -> Result<Value, McpError> {
    tokio::pin!(request);
    let mut open = true;
    loop {
        tokio::select! {
            biased;
            update = updates.recv(), if open => match update {
                Ok(message) => {
                    let Some(params) = message.get("params").filter(|_| {
                        message.get("method").and_then(Value::as_str) == Some("notifications/progress")
                    }) else {
                        continue;
                    };
                    if params.get("progressToken") != Some(token) || operation.check().is_err() {
                        continue;
                    }
                    let finite = |value: &Value| value.as_f64().is_some_and(f64::is_finite);
                    if !params.get("progress").is_some_and(finite)
                        || params.get("total").is_some_and(|total| !finite(total))
                    {
                        continue;
                    }
                    let mut progress = Map::new();
                    progress.insert("progress".into(), params["progress"].clone());
                    if let Some(total) = params.get("total") {
                        progress.insert("total".into(), total.clone());
                    }
                    on_progress(Value::Object(progress));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => open = false,
            },
            result = &mut request => return result,
        }
    }
}

/// `MrtrInputResponse` re-exported for request callers that build continuations.
pub type McpMrtrInputResponse = MrtrInputResponse;
