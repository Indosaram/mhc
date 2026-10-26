//! Rust port of `packages/telemetry-core/src/otlp-inspect.ts`.

use serde_json::Map;
use serde_json::Value;

use crate::otlp_proto::ProtoError;
use crate::otlp_proto::collector_logs::decode_export_logs_service_request;
use crate::otlp_proto::collector_trace::decode_export_trace_service_request;
use crate::otlp_proto::common::AnyValue;
use crate::otlp_proto::common::KeyValue;
use crate::otlp_proto::trace::SPAN_KIND_CLIENT;
use crate::otlp_proto::trace::SPAN_KIND_CONSUMER;
use crate::otlp_proto::trace::SPAN_KIND_INTERNAL;
use crate::otlp_proto::trace::SPAN_KIND_PRODUCER;
use crate::otlp_proto::trace::SPAN_KIND_SERVER;
use crate::otlp_proto::trace::STATUS_CODE_ERROR;
use crate::otlp_proto::trace::STATUS_CODE_OK;
use crate::otlp_spool_file::Signal;

/// `string | number | boolean | undefined` upstream.
pub type InspectedValue = Option<Value>;
pub type InspectedAttributes = Map<String, Value>;

#[derive(Debug, Clone, PartialEq)]
pub struct InspectedLogRecord {
    pub event_name: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub attributes: InspectedAttributes,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InspectedSpan {
    pub name: String,
    pub kind: &'static str,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub status: &'static str,
    pub start_time_unix_nano: u64,
    pub end_time_unix_nano: u64,
    pub attributes: InspectedAttributes,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct InspectedRequest {
    pub logs: Vec<InspectedLogRecord>,
    pub spans: Vec<InspectedSpan>,
}

pub const fn span_kind_name(kind: i32) -> &'static str {
    match kind {
        SPAN_KIND_INTERNAL => "internal",
        SPAN_KIND_SERVER => "server",
        SPAN_KIND_CLIENT => "client",
        SPAN_KIND_PRODUCER => "producer",
        SPAN_KIND_CONSUMER => "consumer",
        _ => "unspecified",
    }
}

pub const fn status_name(code: i32) -> &'static str {
    match code {
        STATUS_CODE_OK => "ok",
        STATUS_CODE_ERROR => "error",
        _ => "unset",
    }
}

fn hex(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    Some(hex::encode(bytes))
}

fn value(any: Option<&AnyValue>) -> InspectedValue {
    match any? {
        AnyValue::Int(number) => Some(Value::from(*number)),
        AnyValue::String(text) => Some(Value::String(text.clone())),
        AnyValue::Bool(flag) => Some(Value::Bool(*flag)),
        AnyValue::Double(number) => Some(Value::from(*number)),
        _ => None,
    }
}

fn attributes(list: &[KeyValue]) -> InspectedAttributes {
    let mut map = InspectedAttributes::new();
    for pair in list {
        let inspected = value(Some(&pair.value));
        map.insert(pair.key.clone(), inspected.unwrap_or(Value::Null));
    }
    map
}

/// Decodes one OTLP/HTTP protobuf request body into plain records with lowercase hex ids, so a QA
/// sink or a test can read what the transport sent.
pub fn inspect_otlp_request(signal: Signal, body: &[u8]) -> Result<InspectedRequest, ProtoError> {
    match signal {
        Signal::Logs => {
            let request = decode_export_logs_service_request(body)?;
            let logs = request
                .resource_logs
                .iter()
                .flat_map(|resource_logs| resource_logs.scope_logs.iter())
                .flat_map(|scope_logs| scope_logs.log_records.iter())
                .map(|record| InspectedLogRecord {
                    event_name: record.event_name.clone(),
                    trace_id: hex(&record.trace_id),
                    span_id: hex(&record.span_id),
                    attributes: attributes(&record.attributes),
                })
                .collect();
            Ok(InspectedRequest {
                logs,
                spans: Vec::new(),
            })
        }
        Signal::Traces => {
            let request = decode_export_trace_service_request(body)?;
            let spans = request
                .resource_spans
                .iter()
                .flat_map(|resource_spans| resource_spans.scope_spans.iter())
                .flat_map(|scope_spans| scope_spans.spans.iter())
                .map(|span| InspectedSpan {
                    name: span.name.clone(),
                    kind: span_kind_name(span.kind),
                    trace_id: hex(&span.trace_id).unwrap_or_default(),
                    span_id: hex(&span.span_id).unwrap_or_default(),
                    parent_span_id: hex(&span.parent_span_id),
                    status: status_name(span.status.as_ref().map_or(0, |status| status.code)),
                    start_time_unix_nano: span.start_time_unix_nano,
                    end_time_unix_nano: span.end_time_unix_nano,
                    attributes: attributes(&span.attributes),
                })
                .collect();
            Ok(InspectedRequest {
                logs: Vec::new(),
                spans,
            })
        }
    }
}
