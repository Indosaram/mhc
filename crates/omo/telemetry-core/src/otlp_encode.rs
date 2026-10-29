//! Rust port of `packages/telemetry-core/src/otlp-encode.ts`.

use serde_json::Map;
use serde_json::Value;

use crate::otel_minimal_logs::ReadableLogRecord;
use crate::otel_minimal_trace::ReadableSpan;
use crate::otel_time::InstrumentationScope;
use crate::otlp_proto::common::AnyValue;
use crate::otlp_proto::common::InstrumentationScope as ProtoInstrumentationScope;
use crate::otlp_proto::common::KeyValue;
use crate::otlp_proto::logs::LogRecord;
use crate::otlp_proto::resource::Resource;
use crate::otlp_proto::trace::Span;
use crate::otlp_proto::trace::Status;

/// `Readonly<Record<string, string | number | boolean>>` in the TS package.
pub type ResourceAttributes = Map<String, Value>;

pub fn hex_to_bytes(hex: &str) -> Vec<u8> {
    let bytes = hex.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut index = 0;
    while index + 1 < bytes.len() {
        let high = hex_digit(bytes[index]);
        let low = hex_digit(bytes[index + 1]);
        out.push((high << 4) | low);
        index += 2;
    }
    out
}

fn hex_digit(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => 0,
    }
}

pub const INT64_MIN: f64 = -9_223_372_036_854_775_808.0;
pub const INT64_LIMIT: f64 = 9_223_372_036_854_775_808.0;

/// `ApiAnyValue` -> proto `AnyValue`; an integer outside int64 has no `int_value` form.
pub fn any_value(value: &Value) -> AnyValue {
    match value {
        Value::String(text) => AnyValue::String(text.clone()),
        Value::Bool(flag) => AnyValue::Bool(*flag),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                return AnyValue::Int(integer);
            }
            match number.as_f64() {
                Some(float) => {
                    if float.fract() == 0.0 && (INT64_MIN..INT64_LIMIT).contains(&float) {
                        AnyValue::Int(float as i64)
                    } else {
                        AnyValue::Double(float)
                    }
                }
                None => AnyValue::Empty,
            }
        }
        Value::Array(values) => AnyValue::Array(values.iter().map(any_value).collect()),
        Value::Object(map) => AnyValue::KvList(key_values(map)),
        Value::Null => AnyValue::Empty,
    }
}

pub fn key_values(attributes: &Map<String, Value>) -> Vec<KeyValue> {
    attributes
        .iter()
        .filter(|(_, value)| !value.is_null())
        .map(|(key, value)| KeyValue {
            key: key.clone(),
            value: any_value(value),
        })
        .collect()
}

pub fn encode_resource(attributes: &ResourceAttributes) -> Resource {
    Resource {
        attributes: key_values(attributes),
        dropped_attributes_count: 0,
    }
}

pub fn encode_scope(scope: &InstrumentationScope) -> ProtoInstrumentationScope {
    ProtoInstrumentationScope {
        name: scope.name.clone(),
        version: scope.version.clone(),
        attributes: Vec::new(),
        dropped_attributes_count: 0,
    }
}

pub fn encode_log_record(record: &ReadableLogRecord) -> LogRecord {
    let span = record.span_context.as_ref();
    LogRecord {
        time_unix_nano: record.time_unix_nano,
        observed_time_unix_nano: record.observed_time_unix_nano,
        severity_number: crate::otlp_proto::logs::severity(record.severity_number),
        severity_text: record.severity_text.clone(),
        event_name: record.event_name.clone(),
        attributes: key_values(&record.attributes),
        dropped_attributes_count: record.dropped_attributes_count,
        body: record.body.clone(),
        trace_id: span.map_or_else(Vec::new, |context| hex_to_bytes(&context.trace_id)),
        span_id: span.map_or_else(Vec::new, |context| hex_to_bytes(&context.span_id)),
        flags: span.map_or(0, |context| u32::from(context.trace_flags)),
    }
}

pub fn encode_span(span: &ReadableSpan) -> Span {    Span {
        trace_id: hex_to_bytes(&span.span_context.trace_id),
        span_id: hex_to_bytes(&span.span_context.span_id),
        trace_state: String::new(),
        parent_span_id: span
            .parent_span_id
            .as_ref()
            .map_or_else(Vec::new, |parent| hex_to_bytes(parent)),
        flags: u32::from(span.span_context.trace_flags),
        name: span.name.clone(),
        kind: span.kind.proto(),
        start_time_unix_nano: span.start_time_unix_nano,
        end_time_unix_nano: span.end_time_unix_nano,
        attributes: key_values(&span.attributes),
        dropped_attributes_count: span.dropped_attributes_count,
        status: Some(Status {
            message: String::new(),
            code: span.status_code.proto(),
        }),
    }
}
