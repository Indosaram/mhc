//! Rust port of `packages/telemetry-core/src/otel-minimal-trace.ts`.
//!
//! The minimal in-house `TracerProvider` behind the `@opentelemetry/api` surface: one
//! instrumentation scope, one processor, an always-on sampler (never `ParentBased`, so an inherited
//! `-00` traceparent never suppresses a span), no span events, links or status messages.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use serde_json::Map;
use serde_json::Value;

use crate::otel_time::MAX_ATTRIBUTES;
use crate::otel_time::OtelClock;
use crate::otel_time::TimeInput;
use crate::otel_time::UnixNanos;
use crate::otel_time::to_unix_nanos;

pub type Attributes = Map<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    Internal,
    Server,
    Client,
    Producer,
    Consumer,
}

impl SpanKind {
    /// The `Span_SpanKind` protobuf value.
    pub const fn proto(self) -> i32 {
        match self {
            Self::Internal => 1,
            Self::Server => 2,
            Self::Client => 3,
            Self::Producer => 4,
            Self::Consumer => 5,
        }
    }

    pub const fn from_proto(value: i32) -> Self {
        match value {
            2 => Self::Server,
            3 => Self::Client,
            4 => Self::Producer,
            5 => Self::Consumer,
            _ => Self::Internal,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::Server => "server",
            Self::Client => "client",
            Self::Producer => "producer",
            Self::Consumer => "consumer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanStatusCode {
    Unset,
    Ok,
    Error,
}

impl SpanStatusCode {
    /// The `Status_StatusCode` protobuf value.
    pub const fn proto(self) -> i32 {
        match self {
            Self::Unset => 0,
            Self::Ok => 1,
            Self::Error => 2,
        }
    }

    pub const fn from_proto(value: i32) -> Self {
        match value {
            1 => Self::Ok,
            2 => Self::Error,
            _ => Self::Unset,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Unset => "unset",
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }
}

pub const TRACE_FLAG_SAMPLED: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanContext {
    /// Lowercase hex, 32 characters.
    pub trace_id: String,
    /// Lowercase hex, 16 characters.
    pub span_id: String,
    pub trace_flags: u8,
}

impl SpanContext {
    pub fn invalid() -> Self {
        Self {
            trace_id: "0".repeat(32),
            span_id: "0".repeat(16),
            trace_flags: 0,
        }
    }

    pub fn is_valid(&self) -> bool {
        is_span_context_valid(self)
    }

    pub fn traceparent(&self) -> String {
        format!(
            "00-{}-{}-{:02x}",
            self.trace_id, self.span_id, self.trace_flags
        )
    }
}

fn is_hex_id(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn is_span_context_valid(context: &SpanContext) -> bool {
    is_hex_id(&context.trace_id, 32)
        && !context.trace_id.bytes().all(|byte| byte == b'0')
        && is_hex_id(&context.span_id, 16)
        && !context.span_id.bytes().all(|byte| byte == b'0')
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReadableSpan {
    pub name: String,
    pub kind: SpanKind,
    pub span_context: SpanContext,
    pub parent_span_id: Option<String>,
    pub start_time_unix_nano: UnixNanos,
    pub end_time_unix_nano: UnixNanos,
    pub attributes: Attributes,
    pub dropped_attributes_count: u32,
    pub status_code: SpanStatusCode,
}

pub trait SpanProcessor: Send + Sync {
    fn on_end(&self, span: &ReadableSpan);
    fn shutdown(&self);
}

#[derive(Debug, Clone, Default)]
pub struct SpanOptions {
    pub kind: Option<SpanKind>,
    pub start_time: Option<TimeInput>,
    pub attributes: Option<Attributes>,
    /// `true` makes the span a root span: no parent is read.
    pub root: bool,
}

fn is_attribute_value(value: &Value) -> bool {
    match value {
        Value::String(_) | Value::Bool(_) => true,
        Value::Number(number) => number.as_f64().is_some_and(f64::is_finite),
        Value::Array(_) => true,
        Value::Null | Value::Object(_) => false,
    }
}

struct SpanState {
    name: String,
    attributes: Attributes,
    dropped: u32,
    status: SpanStatusCode,
    ended: bool,
}

/// One span of the minimal tracer; `end` reports it to the provider's processor exactly once.
pub struct MinimalSpan {
    processor: Option<Arc<dyn SpanProcessor>>,
    clock: OtelClock,
    span_context: SpanContext,
    parent_span_id: Option<String>,
    kind: SpanKind,
    start: UnixNanos,
    state: Mutex<SpanState>,
}

impl MinimalSpan {
    pub fn span_context(&self) -> SpanContext {
        self.span_context.clone()
    }

    pub fn is_recording(&self) -> bool {
        self.processor.is_some()
            && !self
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .ended
    }

    pub fn set_attribute(&self, key: &str, value: Value) {
        if !is_attribute_value(&value) {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.ended {
            return;
        }
        if !state.attributes.contains_key(key) && state.attributes.len() >= MAX_ATTRIBUTES {
            state.dropped += 1;
            return;
        }
        state.attributes.insert(key.to_string(), value);
    }

    pub fn set_attributes(&self, attributes: &Attributes) {
        for (key, value) in attributes {
            self.set_attribute(key, value.clone());
        }
    }

    pub fn set_status(&self, status: SpanStatusCode) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.ended {
            state.status = status;
        }
    }

    pub fn update_name(&self, name: &str) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.ended {
            state.name = name.to_string();
        }
    }

    pub fn end(&self) {
        self.end_at(None);
    }

    pub fn end_at(&self, end_time: Option<TimeInput>) {
        let Some(processor) = self.processor.clone() else {
            return;
        };
        let (name, attributes, dropped, status) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.ended {
                return;
            }
            state.ended = true;
            (
                state.name.clone(),
                state.attributes.clone(),
                state.dropped,
                state.status,
            )
        };
        processor.on_end(&ReadableSpan {
            name,
            kind: self.kind,
            span_context: self.span_context.clone(),
            parent_span_id: self.parent_span_id.clone(),
            start_time_unix_nano: self.start,
            end_time_unix_nano: to_unix_nanos(end_time, &self.clock),
            attributes,
            dropped_attributes_count: dropped,
            status_code: status,
        });
    }
}

fn random_hex_id(bytes: usize) -> String {
    let random = uuid::Uuid::new_v4().into_bytes();
    let mut out = String::with_capacity(bytes * 2);
    for index in 0..bytes {
        let byte = random.get(index % random.len()).copied().unwrap_or(0);
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub struct MinimalTracerProvider {
    processor: Arc<dyn SpanProcessor>,
    clock: OtelClock,
    is_shutdown: AtomicBool,
}

impl MinimalTracerProvider {
    pub fn new(processor: Arc<dyn SpanProcessor>, clock: Option<OtelClock>) -> Self {
        Self {
            processor,
            clock: clock.unwrap_or_else(crate::otel_time::system_clock),
            is_shutdown: AtomicBool::new(false),
        }
    }

    pub fn is_shutdown(&self) -> bool {
        self.is_shutdown.load(Ordering::SeqCst)
    }

    pub fn clock(&self) -> &OtelClock {
        &self.clock
    }

    /// One instrumentation scope per process, so every name gets the same tracer.
    pub fn start_span(
        &self,
        name: &str,
        options: &SpanOptions,
        parent: Option<&SpanContext>,
    ) -> MinimalSpan {
        let parent = if options.root { None } else { parent };
        let valid_parent = parent.filter(|context| is_span_context_valid(context));
        if self.is_shutdown() {
            return MinimalSpan {
                processor: None,
                clock: Arc::clone(&self.clock),
                span_context: SpanContext::invalid(),
                parent_span_id: None,
                kind: SpanKind::Internal,
                start: 0,
                state: Mutex::new(SpanState {
                    name: name.to_string(),
                    attributes: Attributes::new(),
                    dropped: 0,
                    status: SpanStatusCode::Unset,
                    ended: true,
                }),
            };
        }
        let span_context = SpanContext {
            trace_id: valid_parent
                .map_or_else(|| random_hex_id(16), |context| context.trace_id.clone()),
            span_id: random_hex_id(8),
            trace_flags: TRACE_FLAG_SAMPLED,
        };
        let span = MinimalSpan {
            processor: Some(Arc::clone(&self.processor)),
            clock: Arc::clone(&self.clock),
            span_context,
            parent_span_id: valid_parent.map(|context| context.span_id.clone()),
            kind: options.kind.unwrap_or(SpanKind::Internal),
            start: to_unix_nanos(options.start_time, &self.clock),
            state: Mutex::new(SpanState {
                name: name.to_string(),
                attributes: Attributes::new(),
                dropped: 0,
                status: SpanStatusCode::Unset,
                ended: false,
            }),
        };
        if let Some(attributes) = &options.attributes {
            span.set_attributes(attributes);
        }
        span
    }

    pub fn shutdown(&self) {
        if self.is_shutdown.swap(true, Ordering::SeqCst) {
            return;
        }
        self.processor.shutdown();
    }
}
