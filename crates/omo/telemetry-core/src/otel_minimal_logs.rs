//! Rust port of `packages/telemetry-core/src/otel-minimal-logs.ts`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use serde_json::Map;
use serde_json::Value;

use crate::otel_minimal_trace::SpanContext;
use crate::otel_providers::active_span_context;
use crate::otel_time::MAX_ATTRIBUTES;
use crate::otel_time::OtelClock;
use crate::otel_time::TimeInput;
use crate::otel_time::UnixNanos;
use crate::otel_time::to_unix_nanos;
use crate::otlp_proto::common::AnyValue;

pub type LogAttributes = Map<String, Value>;

#[derive(Debug, Clone, PartialEq)]
pub struct ReadableLogRecord {
    pub time_unix_nano: UnixNanos,
    pub observed_time_unix_nano: UnixNanos,
    pub event_name: String,
    pub severity_number: i32,
    pub severity_text: String,
    pub body: Option<AnyValue>,
    pub attributes: LogAttributes,
    pub dropped_attributes_count: u32,
    pub span_context: Option<SpanContext>,
}

pub trait LogRecordProcessor: Send + Sync {
    fn on_emit(&self, record: &ReadableLogRecord);
    fn shutdown(&self);
}

/// The `LogRecord` a caller emits; every optional field defaults like the `api-logs` interface.
#[derive(Debug, Clone, Default)]
pub struct LogRecordInput {
    pub timestamp: Option<TimeInput>,
    pub observed_timestamp: Option<TimeInput>,
    pub event_name: Option<String>,
    pub severity_number: Option<i32>,
    pub severity_text: Option<String>,
    pub body: Option<AnyValue>,
    pub attributes: Option<LogAttributes>,
    pub span_context: Option<SpanContext>,
}

pub struct MinimalLoggerProvider {
    processor: Arc<dyn LogRecordProcessor>,
    clock: OtelClock,
    is_shutdown: AtomicBool,
}

impl MinimalLoggerProvider {
    pub fn new(processor: Arc<dyn LogRecordProcessor>, clock: Option<OtelClock>) -> Self {
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

    pub fn enabled(&self) -> bool {
        !self.is_shutdown()
    }

    pub fn emit(&self, record: &LogRecordInput) {
        if self.is_shutdown() {
            return;
        }
        let span_context = record
            .span_context
            .clone()
            .or_else(active_span_context)
            .filter(|context| context.is_valid());
        let mut attributes = LogAttributes::new();
        let mut dropped = 0u32;
        for (key, value) in record.attributes.iter().flatten() {
            if value.is_null() {
                continue;
            }
            if attributes.len() >= MAX_ATTRIBUTES {
                dropped += 1;
                continue;
            }
            attributes.insert(key.clone(), value.clone());
        }
        self.processor.on_emit(&ReadableLogRecord {
            time_unix_nano: to_unix_nanos(record.timestamp, &self.clock),
            observed_time_unix_nano: to_unix_nanos(record.observed_timestamp, &self.clock),
            event_name: record.event_name.clone().unwrap_or_default(),
            severity_number: record.severity_number.unwrap_or(0),
            severity_text: record.severity_text.clone().unwrap_or_default(),
            body: record.body.clone(),
            attributes,
            dropped_attributes_count: dropped,
            span_context,
        });
    }

    pub fn shutdown(&self) {
        if self.is_shutdown.swap(true, Ordering::SeqCst) {
            return;
        }
        self.processor.shutdown();
    }
}
