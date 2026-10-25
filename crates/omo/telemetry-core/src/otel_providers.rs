//! Rust port of `packages/telemetry-core/src/otel-providers.ts`.
//!
//! Context propagation is the only global state registered; providers stay local to the transport.
//! The Rust port reads the turn context the `otel_turn_span` bridge keeps (a thread-local for
//! synchronous work and a Tokio task-local across awaits), the counterpart of the TS
//! `AsyncLocalStorage` context manager.

pub type OnAppendedCallback = Option<Arc<dyn Fn(&mut SpoolWriter) + Send + Sync>>;
pub type OnWriteErrorCallback = Option<Arc<dyn Fn(&io::Error, usize) + Send + Sync>>;
pub type OnErrorCallback = Arc<dyn Fn(&io::Error, usize) + Send + Sync>;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;


use crate::otel_minimal_logs::LogRecordInput;
use crate::otel_minimal_logs::LogRecordProcessor;
use crate::otel_minimal_logs::MinimalLoggerProvider;
use crate::otel_minimal_logs::ReadableLogRecord;
use crate::otel_minimal_trace::MinimalTracerProvider;
use crate::otel_minimal_trace::ReadableSpan;
use crate::otel_minimal_trace::SpanContext;
use crate::otel_minimal_trace::SpanProcessor;
use crate::otel_time::InstrumentationScope;
use crate::otel_time::OtelClock;
use crate::otel_turn_span::active_turn_traceparent;
use crate::otlp_encode::ResourceAttributes;
use crate::otlp_encode::encode_resource;
use crate::otlp_encode::encode_scope;
use crate::otlp_proto::common::encode_instrumentation_scope;
use crate::otlp_proto::resource::encode_resource_message;
use crate::otlp_spool_file::Signal;
use crate::otlp_spool_file::encode_header;
use crate::otlp_wal::SpanAllowlist;
use crate::otlp_wal::SpoolWriter;
use crate::otlp_wal::SpoolWriterInput;
use crate::otlp_wal::WalLogRecord;
use crate::otlp_wal::WalLogRecordProcessor;
use crate::otlp_wal::WalSpan;
use crate::otlp_wal::WalSpanProcessor;

tokio::task_local! {
    /// The context of a span started through the in-house tracer, for tests and non-turn callers.
    pub static ACTIVE_SPAN_CONTEXT: SpanContext;
}

/// The span context active for this thread/task: the in-house tracer's span first, then the turn
/// traceparent the task bridge publishes.
pub fn active_span_context() -> Option<SpanContext> {
    if let Ok(context) = ACTIVE_SPAN_CONTEXT.try_with(Clone::clone) {
        return Some(context);
    }
    let traceparent = active_turn_traceparent()?;
    Some(SpanContext {
        trace_id: traceparent.get(3..35)?.to_string(),
        span_id: traceparent.get(36..52)?.to_string(),
        trace_flags: u8::from_str_radix(traceparent.get(53..55)?, 16).unwrap_or(0),
    })
}

pub async fn with_span_context<F>(context: SpanContext, future: F) -> F::Output
where
    F: std::future::Future,
{
    ACTIVE_SPAN_CONTEXT.scope(context, future).await
}

pub fn with_span_context_sync<T>(context: SpanContext, run: impl FnOnce() -> T) -> T {
    ACTIVE_SPAN_CONTEXT.sync_scope(context, run)
}

pub struct CreateTelemetryProvidersInput {
    pub resource: ResourceAttributes,
    pub scope: InstrumentationScope,
    pub spool_dir: PathBuf,
    /// The spans the tracer may write and the attributes each may carry; anything else is dropped.
    pub spans: SpanAllowlist,
    pub boot_id: Option<String>,
    pub pid: Option<u32>,
    pub clock: Option<OtelClock>,
    pub consent: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    pub on_appended: OnAppendedCallback,
    pub on_write_error: OnWriteErrorCallback,
}

/// The providers plus the two WAL writers they append to. `seal_*` is the flush's way to publish a
/// batch without going through the processors.
pub struct TelemetryProviders {
    pub logger_provider: Arc<MinimalLoggerProvider>,
    pub tracer_provider: Arc<MinimalTracerProvider>,
    log: Arc<Mutex<WalLogRecordProcessor>>,
    spans: Arc<Mutex<WalSpanProcessor>>,
    pub resource: ResourceAttributes,
    pub scope: InstrumentationScope,
    pub boot_id: String,
}

impl TelemetryProviders {
    pub fn logger(&self) -> &Arc<MinimalLoggerProvider> {
        &self.logger_provider
    }

    pub fn tracer(&self) -> &Arc<MinimalTracerProvider> {
        &self.tracer_provider
    }

    pub fn emit(&self, record: &LogRecordInput) {
        self.logger_provider.emit(record);
    }

    /// Appends one span record through the span processor (the turn-span sink's entry point).
    pub fn append_span(&self, span: &ReadableSpan) {
        self.append_wal_span(&wal_span(span));
    }

    /// Appends an already-converted WAL span (the turn record path).
    pub fn append_wal_span(&self, span: &WalSpan) {
        self.spans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .on_end(span);
    }

    pub fn seal_logs(&self, batch_id: &str) -> io::Result<Option<String>> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .writer
            .seal(batch_id)
    }

    pub fn seal_spans(&self, batch_id: &str) -> io::Result<Option<String>> {
        self.spans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .writer
            .seal(batch_id)
    }

    pub fn log_records(&self) -> usize {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .writer
            .records
    }

    pub fn log_bytes(&self) -> u64 {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .writer
            .bytes
    }

    pub fn span_records(&self) -> usize {
        self.spans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .writer
            .records
    }

    pub fn span_bytes(&self) -> u64 {
        self.spans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .writer
            .bytes
    }

    pub fn shutdown(&self) {
        self.logger_provider.shutdown();
        self.tracer_provider.shutdown();
    }
}

fn hex_id(value: &str) -> Vec<u8> {
    crate::otlp_encode::hex_to_bytes(value)
}

/// The `api-logs` record as the WAL's own log record.
pub fn wal_log_record(record: &ReadableLogRecord) -> WalLogRecord {
    let span = record.span_context.as_ref();
    WalLogRecord {
        event_name: Some(record.event_name.clone()),
        time_unix_nano: record.time_unix_nano,
        observed_time_unix_nano: record.observed_time_unix_nano,
        severity_number: u32::try_from(record.severity_number).unwrap_or(0),
        attributes: record.attributes.clone(),
        dropped_attributes_count: record.dropped_attributes_count,
        trace_id: span.map_or_else(Vec::new, |context| hex_id(&context.trace_id)),
        span_id: span.map_or_else(Vec::new, |context| hex_id(&context.span_id)),
        trace_flags: span.map_or(0, |context| context.trace_flags),
    }
}

/// The `api` span as the WAL's own span record; `kind` is zero-based there, the proto value minus one.
pub fn wal_span(span: &ReadableSpan) -> WalSpan {
    WalSpan {
        name: span.name.clone(),
        trace_id: hex_id(&span.span_context.trace_id),
        span_id: hex_id(&span.span_context.span_id),
        parent_span_id: span
            .parent_span_id
            .as_ref()
            .map_or_else(Vec::new, |parent| hex_id(parent)),
        trace_flags: span.span_context.trace_flags,
        kind: u32::try_from(span.kind.proto()).unwrap_or(1).saturating_sub(1),
        start_time_unix_nano: span.start_time_unix_nano,
        end_time_unix_nano: span.end_time_unix_nano,
        attributes: span.attributes.clone(),
        dropped_attributes_count: span.dropped_attributes_count,
        status_code: u32::try_from(span.status_code.proto()).unwrap_or(0),
    }
}

struct LogAdapter {
    processor: Arc<Mutex<WalLogRecordProcessor>>,
}

impl LogRecordProcessor for LogAdapter {
    fn on_emit(&self, record: &ReadableLogRecord) {
        let mut processor = self
            .processor
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        processor.on_emit(&wal_log_record(record));
    }

    fn shutdown(&self) {
        self.processor
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .shutdown();
    }
}

struct SpanAdapter {
    processor: Arc<Mutex<WalSpanProcessor>>,
}

impl SpanProcessor for SpanAdapter {
    fn on_end(&self, span: &ReadableSpan) {
        let mut processor = self
            .processor
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        processor.on_end(&wal_span(span));
    }

    fn shutdown(&self) {
        self.processor
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .shutdown();
    }
}

pub fn create_boot_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

pub fn create_telemetry_providers(input: CreateTelemetryProvidersInput) -> TelemetryProviders {
    let boot_id = input.boot_id.clone().unwrap_or_else(create_boot_id);
    let pid = input.pid.unwrap_or_else(std::process::id);
    let consent: Arc<dyn Fn() -> bool + Send + Sync> =
        input.consent.unwrap_or_else(|| Arc::new(|| true));
    let on_error: OnErrorCallback = input
        .on_write_error
        .unwrap_or_else(|| Arc::new(|_error: &io::Error, _records: usize| {}));
    let on_appended: Arc<dyn Fn(&mut SpoolWriter) + Send + Sync> = input
        .on_appended
        .unwrap_or_else(|| Arc::new(|_writer: &mut SpoolWriter| {}));
    let resource_bytes = encode_resource_message(&encode_resource(&input.resource));
    let scope_bytes = encode_instrumentation_scope(&encode_scope(&input.scope));
    let log = Arc::new(Mutex::new(WalLogRecordProcessor::new(
        SpoolWriter::new(SpoolWriterInput {
            dir: input.spool_dir.clone(),
            signal: Signal::Logs,
            pid,
            boot_id: boot_id.clone(),
            header: encode_header(Signal::Logs, &resource_bytes, &scope_bytes),
            consent: Arc::clone(&consent),
            on_error: Arc::clone(&on_error),
            write: None,
            truncate: None,
        }),
        {
            let on_appended = Arc::clone(&on_appended);
            Box::new(move |writer: &mut SpoolWriter| on_appended(writer))
        },
    )));
    let spans = Arc::new(Mutex::new(WalSpanProcessor::new(
        SpoolWriter::new(SpoolWriterInput {
            dir: input.spool_dir.clone(),
            signal: Signal::Traces,
            pid,
            boot_id: boot_id.clone(),
            header: encode_header(Signal::Traces, &resource_bytes, &scope_bytes),
            consent: Arc::clone(&consent),
            on_error: Arc::clone(&on_error),
            write: None,
            truncate: None,
        }),
        {
            let on_appended = Arc::clone(&on_appended);
            Box::new(move |writer: &mut SpoolWriter| on_appended(writer))
        },
        input.spans,
    )));
    let clock = input.clock.clone();
    let logger_provider = Arc::new(MinimalLoggerProvider::new(
        Arc::new(LogAdapter {
            processor: Arc::clone(&log),
        }),
        clock.clone(),
    ));
    let tracer_provider = Arc::new(MinimalTracerProvider::new(
        Arc::new(SpanAdapter {
            processor: Arc::clone(&spans),
        }),
        clock,
    ));
    TelemetryProviders {
        logger_provider,
        tracer_provider,
        log,
        spans,
        resource: input.resource,
        scope: input.scope,
        boot_id,
    }
}

/// The trace/span pair of the span active in the current task, if it is valid.
pub fn get_active_trace_context() -> Option<SpanContext> {
    active_span_context().filter(SpanContext::is_valid)
}
