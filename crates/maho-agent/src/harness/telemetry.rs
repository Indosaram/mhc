//! Port of senpi packages/agent/src/harness/telemetry.ts over senpi packages/telemetry/src.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;
use serde_json::{Map, Value, json};

use super::context::{Context, get_telemetry_context, with_telemetry_context};

/// `AttributeValue`.
pub type AttributeValue = Value;

/// `SpanAttributes`.
pub type SpanAttributes = Map<String, Value>;

/// `SpanOptions`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpanOptions {
    pub name: String,
    pub attributes: SpanAttributes,
}

/// `SpanStatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpanStatus {
    Ok,
    Error { error: Option<SpanStatusError> },
}

/// `SpanStatus.error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanStatusError {
    pub name: String,
    pub message: String,
}

/// One recorded event on a span.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedTelemetryEvent {
    pub name: String,
    pub attributes: SpanAttributes,
}

/// `RecordedTelemetrySpan`.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedTelemetrySpan {
    pub id: u64,
    pub parent_id: Option<u64>,
    pub name: String,
    pub attributes: SpanAttributes,
    pub events: Vec<RecordedTelemetryEvent>,
    pub status: SpanStatus,
    pub settled: bool,
    pub end_sequence: Option<u64>,
}

/// The span runner a backend invokes with the started span.
pub type SpanRunner = Box<dyn FnOnce(TelemetrySpan) -> BoxFuture<'static, ()> + Send>;

/// The backend contract shared by `TelemetryContext` and `TelemetrySpan`.
pub trait Telemetry: Send + Sync {
    fn start_span_raw(&self, options: SpanOptions, callback: SpanRunner) -> BoxFuture<'static, ()>;
    fn add_event(&self, name: &str, attributes: SpanAttributes);
    fn set_attributes(&self, attributes: SpanAttributes);
    fn set_status(&self, status: SpanStatus);
}

/// `TelemetryContext`.
#[derive(Clone)]
pub struct TelemetryContext(Arc<dyn Telemetry>);

impl std::fmt::Debug for TelemetryContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TelemetryContext")
    }
}

impl TelemetryContext {
    pub fn new(inner: Arc<dyn Telemetry>) -> Self {
        Self(inner)
    }

    pub fn from_span(span: &TelemetrySpan) -> Self {
        Self(span.0.clone())
    }

    pub fn backend(&self) -> &Arc<dyn Telemetry> {
        &self.0
    }

    /// `startSpan(options, callback)`.
    pub async fn start_span<T, F>(&self, options: SpanOptions, callback: F) -> T
    where
        T: Send + 'static,
        F: FnOnce(TelemetrySpan) -> BoxFuture<'static, T> + Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let runner: SpanRunner = Box::new(move |span| {
            Box::pin(async move {
                let value = callback(span).await;
                let _ = tx.send(value);
            })
        });
        self.0.start_span_raw(options, runner).await;
        rx.await.expect("telemetry callback always runs")
    }
}

/// `TelemetrySpan`.
#[derive(Clone)]
pub struct TelemetrySpan(Arc<dyn Telemetry>);

impl std::fmt::Debug for TelemetrySpan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TelemetrySpan")
    }
}

impl TelemetrySpan {
    pub fn new(inner: Arc<dyn Telemetry>) -> Self {
        Self(inner)
    }

    /// `span.startSpan(options, callback)`.
    pub async fn start_span<T, F>(&self, options: SpanOptions, callback: F) -> T
    where
        T: Send + 'static,
        F: FnOnce(TelemetrySpan) -> BoxFuture<'static, T> + Send + 'static,
    {
        TelemetryContext(self.0.clone()).start_span(options, callback).await
    }

    /// `span.addEvent(name, attributes?)`.
    pub fn add_event(&self, name: &str, attributes: SpanAttributes) {
        self.0.add_event(name, attributes);
    }

    /// `span.setAttributes(attributes)`.
    pub fn set_attributes(&self, attributes: SpanAttributes) {
        self.0.set_attributes(attributes);
    }

    /// `span.setStatus(status)`.
    pub fn set_status(&self, status: SpanStatus) {
        self.0.set_status(status);
    }

    pub fn as_context(&self) -> TelemetryContext {
        TelemetryContext(self.0.clone())
    }
}

/// `NOOP_TELEMETRY_CONTEXT`.
pub fn noop_telemetry_context() -> TelemetryContext {
    static CONTEXT: std::sync::OnceLock<TelemetryContext> = std::sync::OnceLock::new();
    CONTEXT.get_or_init(|| TelemetryContext(Arc::new(NoopTelemetry))).clone()
}

struct NoopTelemetry;

impl Telemetry for NoopTelemetry {
    fn start_span_raw(&self, _options: SpanOptions, callback: SpanRunner) -> BoxFuture<'static, ()> {
        Box::pin(async move { callback(TelemetrySpan(Arc::new(NoopTelemetry))).await })
    }

    fn add_event(&self, _name: &str, _attributes: SpanAttributes) {}

    fn set_attributes(&self, _attributes: SpanAttributes) {}

    fn set_status(&self, _status: SpanStatus) {}
}

struct InMemoryState {
    spans: Vec<MutableSpan>,
    next_span_id: u64,
    next_end_sequence: u64,
}

struct MutableSpan {
    id: u64,
    parent_id: Option<u64>,
    name: String,
    attributes: SpanAttributes,
    events: Vec<RecordedTelemetryEvent>,
    status: SpanStatus,
    explicit_status: bool,
    settled: bool,
    end_sequence: Option<u64>,
}

struct InMemoryTelemetry {
    state: Arc<Mutex<InMemoryState>>,
    span: Option<usize>,
}

fn copy_attributes(attributes: &SpanAttributes) -> SpanAttributes {
    let mut copy = SpanAttributes::new();
    for (name, value) in attributes {
        if !value.is_null() {
            copy.insert(name.clone(), value.clone());
        }
    }
    copy
}

fn merge_attributes(current: &SpanAttributes, attributes: &SpanAttributes) -> SpanAttributes {
    let mut merged = copy_attributes(current);
    for (name, value) in attributes {
        if !value.is_null() {
            merged.insert(name.clone(), value.clone());
        }
    }
    merged
}

fn copy_status(status: &SpanStatus) -> SpanStatus {
    status.clone()
}

fn automatic_error_status(error: Option<&str>) -> SpanStatus {
    match error {
        Some(message) => SpanStatus::Error {
            error: Some(SpanStatusError { name: "Error".to_string(), message: message.to_string() }),
        },
        None => SpanStatus::Error { error: None },
    }
}

fn settle_span(state: &mut InMemoryState, index: usize, failed: bool, error: Option<&str>) {
    let end_sequence = state.next_end_sequence;
    let span = &mut state.spans[index];
    if span.settled {
        return;
    }
    if failed && !span.explicit_status {
        span.status = automatic_error_status(error);
    }
    span.settled = true;
    span.end_sequence = Some(end_sequence);
    state.next_end_sequence += 1;
}

impl Telemetry for InMemoryTelemetry {
    fn start_span_raw(&self, options: SpanOptions, callback: SpanRunner) -> BoxFuture<'static, ()> {
        let state = self.state.clone();
        let parent = self.span;
        Box::pin(async move {
            let parent_settled = parent.is_some_and(|index| {
                let guard = state.lock().expect("telemetry state poisoned");
                guard.spans[index].settled
            });
            if parent_settled {
                return NoopTelemetry.start_span_raw(options, callback).await;
            }

            let index = {
                let mut guard = state.lock().expect("telemetry state poisoned");
                let id = guard.next_span_id;
                guard.next_span_id += 1;
                let parent_id = parent.map(|parent| guard.spans[parent].id);
                guard.spans.push(MutableSpan {
                    id,
                    parent_id,
                    name: options.name.clone(),
                    attributes: copy_attributes(&options.attributes),
                    events: Vec::new(),
                    status: SpanStatus::Ok,
                    explicit_status: false,
                    settled: false,
                    end_sequence: None,
                });
                guard.spans.len() - 1
            };

            let span = TelemetrySpan(Arc::new(InMemoryTelemetry { state: state.clone(), span: Some(index) }));
            callback(span).await;

            let mut guard = state.lock().expect("telemetry state poisoned");
            settle_span(&mut guard, index, false, None);
        })
    }

    fn add_event(&self, name: &str, attributes: SpanAttributes) {
        let Some(index) = self.span else { return };
        let mut guard = self.state.lock().expect("telemetry state poisoned");
        if guard.spans[index].settled {
            return;
        }
        guard.spans[index].events.push(RecordedTelemetryEvent {
            name: name.to_string(),
            attributes: copy_attributes(&attributes),
        });
    }

    fn set_attributes(&self, attributes: SpanAttributes) {
        let Some(index) = self.span else { return };
        let mut guard = self.state.lock().expect("telemetry state poisoned");
        if guard.spans[index].settled {
            return;
        }
        let merged = merge_attributes(&guard.spans[index].attributes, &attributes);
        guard.spans[index].attributes = merged;
    }

    fn set_status(&self, status: SpanStatus) {
        let Some(index) = self.span else { return };
        let mut guard = self.state.lock().expect("telemetry state poisoned");
        if guard.spans[index].settled {
            return;
        }
        guard.spans[index].status = copy_status(&status);
        guard.spans[index].explicit_status = true;
    }
}

/// `InMemoryTelemetryContext`: backend-neutral reference implementation recording spans in memory.
#[derive(Clone)]
pub struct InMemoryTelemetryContext {
    state: Arc<Mutex<InMemoryState>>,
}

impl Default for InMemoryTelemetryContext {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryTelemetryContext {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(InMemoryState {
                spans: Vec::new(),
                next_span_id: 1,
                next_end_sequence: 1,
            })),
        }
    }

    pub fn context(&self) -> TelemetryContext {
        TelemetryContext(Arc::new(InMemoryTelemetry { state: self.state.clone(), span: None }))
    }

    /// `getSpans()`: detached snapshots in span-start order.
    pub fn get_spans(&self) -> Vec<RecordedTelemetrySpan> {
        let guard = self.state.lock().expect("telemetry state poisoned");
        guard
            .spans
            .iter()
            .map(|span| RecordedTelemetrySpan {
                id: span.id,
                parent_id: span.parent_id,
                name: span.name.clone(),
                attributes: copy_attributes(&span.attributes),
                events: span
                    .events
                    .iter()
                    .map(|event| RecordedTelemetryEvent {
                        name: event.name.clone(),
                        attributes: copy_attributes(&event.attributes),
                    })
                    .collect(),
                status: copy_status(&span.status),
                settled: span.settled,
                end_sequence: span.end_sequence,
            })
            .collect()
    }
}

/// `startAiSpan(name, attributes, callback, context)`.
pub async fn start_ai_span<T, F>(name: &str, attributes: SpanAttributes, callback: F, context: &Context) -> T
where
    T: Send + 'static,
    F: FnOnce(TelemetrySpan, Context) -> BoxFuture<'static, T> + Send + 'static,
{
    let telemetry_context = get_telemetry_context(context);
    let parent = context.clone();
    telemetry_context
        .start_span(SpanOptions { name: name.to_string(), attributes }, move |span| {
            let child_context = with_telemetry_context(TelemetryContext::from_span(&span), &parent);
            callback(span, child_context)
        })
        .await
}

/// `startHarnessSpan(name, attributes, callback, context)`.
pub async fn start_harness_span<T, F>(name: &str, attributes: SpanAttributes, callback: F, context: &Context) -> T
where
    T: Send + 'static,
    F: FnOnce(TelemetrySpan, Context) -> BoxFuture<'static, T> + Send + 'static,
{
    start_ai_span(name, attributes, callback, context).await
}

/// `HOOK_NAMES`.
pub const HOOK_NAMES: &[&str] = &[
    "before_run",
    "before_drive",
    "before_run_end",
    "transform_context",
    "before_request",
    "before_payload",
    "after_response",
    "before_tool",
    "after_tool",
    "before_compaction",
    "before_navigation",
];

/// `EVENT_TYPES`.
pub const EVENT_TYPES: &[&str] = &[
    "run_start",
    "run_resume",
    "run_suspend",
    "operation_abort",
    "run_end",
    "fault",
    "handler_error",
    "turn_start",
    "turn_end",
    "retry_scheduled",
    "retry_start",
    "retry_end",
    "message_start",
    "message_update",
    "message_end",
    "tool_start",
    "tool_update",
    "tool_end",
    "entry_added",
    "queue_update",
    "value_update",
    "config_update",
    "compaction_start",
    "compaction_end",
    "navigation_start",
    "navigation_end",
    "lane_created",
    "usage",
];

/// `HARNESS_TELEMETRY_SCHEMA` span names, in declaration order.
pub const HARNESS_SPAN_NAMES: &[&str] = &[
    "pi.harness.run",
    "pi.harness.compaction",
    "pi.harness.navigation",
    "pi.harness.checkpoint",
    "pi.harness.turn",
    "pi.harness.step",
    "pi.harness.tool",
    "pi.harness.hook",
    "pi.harness.sleep",
    "pi.harness.event_handler",
    "pi.session.write",
];

/// `AI_TELEMETRY_SCHEMA`.
pub fn ai_telemetry_schema() -> Value {
    json!({
        "version": 1,
        "spans": {
            "pi.ai.request": {
                "description": "One logical request to an AI provider",
                "parents": { "kind": "any" },
                "startAttributes": {
                    "pi.ai.operation": {
                        "type": "string",
                        "required": true,
                        "values": ["stream", "fetch_deferred", "cancel_deferred", "generate_images"],
                        "description": "Logical provider operation"
                    },
                    "pi.ai.provider": { "type": "string", "required": true, "description": "Selected provider id" },
                    "pi.ai.model": { "type": "string", "required": true, "description": "Requested model id" },
                    "pi.ai.api": { "type": "string", "required": true, "description": "Provider API id" },
                    "pi.ai.streaming": {
                        "type": "boolean",
                        "required": true,
                        "description": "Whether this operation returns a stream"
                    },
                    "pi.ai.deferred": {
                        "type": "boolean",
                        "required": false,
                        "description": "Whether the operation requests or participates in deferred execution"
                    }
                },
                "endAttributes": {
                    "pi.ai.response.model": { "type": "string", "description": "Concrete response model" },
                    "pi.ai.response.id": {
                        "type": "string",
                        "cardinality": "high",
                        "description": "Provider response id"
                    },
                    "pi.ai.response.stop_reason": {
                        "type": "string",
                        "values": ["stop", "length", "tool_use", "error", "aborted", "deferred"],
                        "description": "Normalized terminal response reason"
                    },
                    "pi.ai.http.status_code": { "type": "number", "description": "Final HTTP status" },
                    "pi.ai.usage.input_tokens": { "type": "number", "description": "Reported input tokens" },
                    "pi.ai.usage.output_tokens": { "type": "number", "description": "Reported output tokens" },
                    "pi.ai.usage.cache_read_tokens": { "type": "number", "description": "Reported cache-read tokens" },
                    "pi.ai.usage.cache_write_tokens": { "type": "number", "description": "Reported cache-write tokens" },
                    "pi.ai.usage.reasoning_tokens": { "type": "number", "description": "Reported reasoning tokens" },
                    "pi.ai.usage.total_tokens": { "type": "number", "description": "Reported total tokens" },
                    "pi.ai.usage.cost": { "type": "number", "description": "Reported total cost" },
                    "pi.ai.stream.chunk_count": { "type": "number", "description": "Streamed update chunk count" },
                    "pi.ai.stream.time_to_first_chunk_ms": {
                        "type": "number",
                        "description": "Elapsed milliseconds to first update chunk"
                    },
                    "pi.ai.error.type": {
                        "type": "string",
                        "cardinality": "low",
                        "description": "Provider or transport error class"
                    }
                },
                "status": { "default": "ok", "errorWhen": "The operation throws or returns an error result" }
            }
        }
    })
}

fn operation_start_attributes() -> Value {
    json!({
        "pi.session.id": { "type": "string", "required": true, "cardinality": "high", "description": "Session id" },
        "pi.lane.name": { "type": "string", "required": true, "cardinality": "high", "description": "Lane name" },
        "pi.operation.id": {
            "type": "string",
            "required": true,
            "cardinality": "high",
            "description": "Durable operation id"
        },
        "pi.operation.recovery": {
            "type": "boolean",
            "required": true,
            "description": "Whether this invocation resumes durable work"
        }
    })
}

fn operation_error_attributes() -> Value {
    json!({
        "pi.error.code": {
            "type": "string",
            "cardinality": "low",
            "description": "Stable operation error code"
        },
        "pi.error.type": {
            "type": "string",
            "cardinality": "low",
            "description": "Low-cardinality operation error class"
        }
    })
}

fn operation_start_attributes_with_kind(kind: &str, description: &str) -> Value {
    let mut map = operation_start_attributes().as_object().cloned().unwrap_or_default();
    map.insert(
        "pi.operation.kind".to_string(),
        json!({ "type": "string", "required": true, "values": [kind], "description": description }),
    );
    Value::Object(map)
}

fn operation_end_attributes(outcomes: &[&str], description: &str) -> Value {
    let mut map = operation_error_attributes().as_object().cloned().unwrap_or_default();
    map.insert(
        "pi.operation.outcome".to_string(),
        json!({ "type": "string", "values": outcomes, "description": description }),
    );
    Value::Object(map)
}

fn lane_and_operation_attributes() -> Value {
    json!({
        "pi.lane.name": { "type": "string", "required": true, "cardinality": "high", "description": "Lane name" },
        "pi.operation.id": {
            "type": "string",
            "required": true,
            "cardinality": "high",
            "description": "Durable operation id"
        }
    })
}

/// `HARNESS_TELEMETRY_SCHEMA`.
pub fn harness_telemetry_schema() -> Value {
    let mut spans = Map::new();

    spans.insert(
        "pi.harness.run".to_string(),
        json!({
            "description": "One admitted in-process run invocation",
            "parents": { "kind": "root_or_external" },
            "startAttributes": operation_start_attributes_with_kind("run", "Run operation kind"),
            "endAttributes": operation_end_attributes(
                &["completed", "aborted", "failed", "suspended"],
                "Run invocation outcome"
            ),
            "status": { "default": "ok", "errorWhen": "The run fails or throws" }
        }),
    );
    spans.insert(
        "pi.harness.compaction".to_string(),
        json!({
            "description": "One admitted in-process manual compaction invocation",
            "parents": { "kind": "root_or_external" },
            "startAttributes": operation_start_attributes_with_kind("compaction", "Compaction operation kind"),
            "endAttributes": operation_end_attributes(
                &["completed", "declined", "aborted", "failed"],
                "Compaction invocation outcome"
            ),
            "status": { "default": "ok", "errorWhen": "The compaction fails or throws" }
        }),
    );
    spans.insert(
        "pi.harness.navigation".to_string(),
        json!({
            "description": "One admitted in-process navigation invocation",
            "parents": { "kind": "root_or_external" },
            "startAttributes": operation_start_attributes_with_kind("navigation", "Navigation operation kind"),
            "endAttributes": operation_end_attributes(
                &["completed", "declined", "aborted", "failed"],
                "Navigation invocation outcome"
            ),
            "status": { "default": "ok", "errorWhen": "The navigation fails or throws" }
        }),
    );
    spans.insert(
        "pi.harness.checkpoint".to_string(),
        json!({
            "description": "One run checkpoint",
            "parents": { "kind": "spans", "spans": ["pi.harness.run"] },
            "startAttributes": {
                "pi.lane.name": {
                    "type": "string",
                    "required": true,
                    "cardinality": "high",
                    "description": "Lane name"
                },
                "pi.operation.id": {
                    "type": "string",
                    "required": true,
                    "cardinality": "high",
                    "description": "Durable operation id"
                },
                "pi.checkpoint.kind": {
                    "type": "string",
                    "required": true,
                    "values": ["normal", "abort_reconcile"],
                    "description": "Checkpoint purpose"
                }
            },
            "endAttributes": {},
            "status": { "default": "ok", "errorWhen": "Checkpoint work throws" }
        }),
    );
    let mut turn_start = lane_and_operation_attributes().as_object().cloned().unwrap_or_default();
    turn_start.insert(
        "pi.turn.id".to_string(),
        json!({ "type": "string", "required": true, "cardinality": "high", "description": "Invocation-local turn id" }),
    );
    spans.insert(
        "pi.harness.turn".to_string(),
        json!({
            "description": "One assistant response and its tool batch",
            "parents": { "kind": "spans", "spans": ["pi.harness.run"] },
            "startAttributes": Value::Object(turn_start),
            "endAttributes": {},
            "status": { "default": "ok", "errorWhen": "Turn work throws" }
        }),
    );
    let mut step_start = lane_and_operation_attributes().as_object().cloned().unwrap_or_default();
    step_start.insert(
        "pi.step.kind".to_string(),
        json!({
            "type": "string",
            "required": true,
            "values": ["assistant", "compaction", "branch_summary"],
            "description": "Retryable step kind"
        }),
    );
    step_start.insert(
        "pi.step.attempt".to_string(),
        json!({ "type": "number", "required": true, "description": "One-based durable attempt number" }),
    );
    step_start.insert(
        "pi.compaction.reason".to_string(),
        json!({
            "type": "string",
            "required": false,
            "values": ["manual", "threshold", "overflow"],
            "description": "Compaction trigger"
        }),
    );
    spans.insert(
        "pi.harness.step".to_string(),
        json!({
            "description": "One durable retry attempt",
            "parents": {
                "kind": "spans",
                "spans": [
                    "pi.harness.turn",
                    "pi.harness.checkpoint",
                    "pi.harness.compaction",
                    "pi.harness.navigation"
                ]
            },
            "startAttributes": Value::Object(step_start),
            "endAttributes": {
                "pi.step.outcome": {
                    "type": "string",
                    "values": ["succeeded", "retry", "failed", "aborted", "deferred", "overflow"],
                    "description": "Attempt outcome"
                }
            },
            "status": { "default": "ok", "errorWhen": "The attempt retries, fails, or throws" }
        }),
    );
    spans.insert(
        "pi.harness.tool".to_string(),
        json!({
            "description": "One raw phase-2 tool execution",
            "parents": { "kind": "spans", "spans": ["pi.harness.turn", "pi.harness.run"] },
            "startAttributes": {
                "pi.lane.name": {
                    "type": "string",
                    "required": true,
                    "cardinality": "high",
                    "description": "Lane name"
                },
                "pi.operation.id": {
                    "type": "string",
                    "required": true,
                    "cardinality": "high",
                    "description": "Durable operation id"
                },
                "pi.turn.id": {
                    "type": "string",
                    "required": false,
                    "cardinality": "high",
                    "description": "Invocation-local live turn id"
                },
                "pi.tool.name": { "type": "string", "required": true, "description": "Tool name" },
                "pi.tool.call_id": {
                    "type": "string",
                    "required": true,
                    "cardinality": "high",
                    "description": "Tool call id"
                },
                "pi.tool.replay": {
                    "type": "string",
                    "required": true,
                    "values": ["never", "safe"],
                    "description": "Declared replay policy"
                },
                "pi.tool.recovery": {
                    "type": "boolean",
                    "required": true,
                    "description": "Whether this is recovery execution"
                }
            },
            "endAttributes": {
                "pi.tool.is_error": {
                    "type": "boolean",
                    "description": "Whether raw phase-2 execution returned an error"
                }
            },
            "status": { "default": "ok", "errorWhen": "Raw phase-2 execution returns an error" }
        }),
    );
    spans.insert(
        "pi.harness.hook".to_string(),
        json!({
            "description": "One registered hook handler invocation",
            "parents": { "kind": "any" },
            "startAttributes": {
                "pi.lane.name": {
                    "type": "string",
                    "required": true,
                    "cardinality": "high",
                    "description": "Lane name"
                },
                "pi.operation.id": {
                    "type": "string",
                    "required": false,
                    "cardinality": "high",
                    "description": "Durable operation id when accepted"
                },
                "pi.hook.name": {
                    "type": "string",
                    "required": true,
                    "values": HOOK_NAMES,
                    "description": "Hook name"
                },
                "pi.hook.registration_id": {
                    "type": "string",
                    "required": false,
                    "description": "Optional hook registration metadata"
                }
            },
            "endAttributes": {
                "pi.hook.outcome": {
                    "type": "string",
                    "values": ["completed", "skipped", "blocked", "failed"],
                    "description": "Handler outcome"
                }
            },
            "status": { "default": "ok", "errorWhen": "The handler throws" }
        }),
    );
    spans.insert(
        "pi.harness.sleep".to_string(),
        json!({
            "description": "One retry delay",
            "parents": {
                "kind": "spans",
                "spans": [
                    "pi.harness.run",
                    "pi.harness.compaction",
                    "pi.harness.navigation",
                    "pi.harness.turn",
                    "pi.harness.checkpoint"
                ]
            },
            "startAttributes": {
                "pi.operation.id": {
                    "type": "string",
                    "required": true,
                    "cardinality": "high",
                    "description": "Durable operation id"
                },
                "pi.sleep.delay_ms": {
                    "type": "number",
                    "required": true,
                    "description": "Requested delay in milliseconds"
                }
            },
            "endAttributes": {
                "pi.sleep.outcome": {
                    "type": "string",
                    "values": ["elapsed", "aborted"],
                    "description": "Delay outcome"
                }
            },
            "status": { "default": "ok", "errorWhen": "Sleep work throws" }
        }),
    );
    spans.insert(
        "pi.harness.event_handler".to_string(),
        json!({
            "description": "One passive event listener invocation",
            "parents": { "kind": "any" },
            "startAttributes": {
                "pi.event.type": {
                    "type": "string",
                    "required": true,
                    "cardinality": "low",
                    "values": EVENT_TYPES,
                    "description": "Delivered harness event type"
                },
                "pi.lane.name": {
                    "type": "string",
                    "required": false,
                    "cardinality": "high",
                    "description": "Lane name for lane-scoped events"
                }
            },
            "endAttributes": {},
            "status": { "default": "ok", "errorWhen": "The listener throws" }
        }),
    );
    spans.insert(
        "pi.session.write".to_string(),
        json!({
            "description": "One committed session transaction",
            "parents": { "kind": "any" },
            "startAttributes": {
                "pi.session.id": {
                    "type": "string",
                    "required": true,
                    "cardinality": "high",
                    "description": "Session id"
                },
                "pi.lane.name": {
                    "type": "string",
                    "required": false,
                    "cardinality": "high",
                    "description": "Lane name when supplied by the caller"
                },
                "pi.operation.id": {
                    "type": "string",
                    "required": false,
                    "cardinality": "high",
                    "description": "Durable operation id when supplied by the caller"
                },
                "pi.session.item_count": {
                    "type": "number",
                    "required": true,
                    "description": "Number of writes in the transaction"
                },
                "pi.session.item_kinds": {
                    "type": "string[]",
                    "required": true,
                    "elementValues": ["entry", "usage", "value", "list"],
                    "description": "Distinct write kinds in the transaction"
                }
            },
            "endAttributes": {
                "pi.session.first_seq": {
                    "type": "number",
                    "description": "First committed sequence in the transaction"
                },
                "pi.session.last_seq": {
                    "type": "number",
                    "description": "Last committed sequence in the transaction"
                }
            },
            "status": { "default": "ok", "errorWhen": "Storage rejects the transaction" }
        }),
    );

    Value::Object(Map::from_iter([("version".to_string(), json!(1)), ("spans".to_string(), Value::Object(spans))]))
}

/// `AGENT_TELEMETRY_SCHEMAS`.
pub fn agent_telemetry_schemas() -> Vec<Value> {
    vec![ai_telemetry_schema(), harness_telemetry_schema()]
}

/// Attribute metadata index used by the schema-doc renderer.
pub type TelemetryAttributeMetadata = BTreeMap<String, Value>;
