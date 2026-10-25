//! Rust port of `otlp-proto/opentelemetry/proto/trace/v1/trace_pb.ts`.

use super::ProtoError;
use super::Reader;
use super::WIRE_FIXED32;
use super::WIRE_FIXED64;
use super::WIRE_LEN;
use super::WIRE_VARINT;
use super::common::InstrumentationScope;
use super::common::KeyValue;
use super::common::decode_instrumentation_scope;
use super::common::decode_key_value;
use super::common::encode_instrumentation_scope;
use super::common::encode_key_value;
use super::resource::Resource;
use super::resource::decode_resource_message;
use super::resource::encode_resource_message;
use super::write_bytes;
use super::write_enum;
use super::write_fixed32;
use super::write_fixed64;
use super::write_message;
use super::write_string;
use super::write_uint;

pub const SPAN_KIND_UNSPECIFIED: i32 = 0;
pub const SPAN_KIND_INTERNAL: i32 = 1;
pub const SPAN_KIND_SERVER: i32 = 2;
pub const SPAN_KIND_CLIENT: i32 = 3;
pub const SPAN_KIND_PRODUCER: i32 = 4;
pub const SPAN_KIND_CONSUMER: i32 = 5;

pub const STATUS_CODE_UNSET: i32 = 0;
pub const STATUS_CODE_OK: i32 = 1;
pub const STATUS_CODE_ERROR: i32 = 2;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub message: String,
    pub code: i32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Span {
    pub trace_id: Vec<u8>,
    pub span_id: Vec<u8>,
    pub trace_state: String,
    pub parent_span_id: Vec<u8>,
    pub flags: u32,
    pub name: String,
    pub kind: i32,
    pub start_time_unix_nano: u64,
    pub end_time_unix_nano: u64,
    pub attributes: Vec<KeyValue>,
    pub dropped_attributes_count: u32,
    pub status: Option<Status>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScopeSpans {
    pub scope: Option<InstrumentationScope>,
    pub spans: Vec<Span>,
    pub schema_url: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResourceSpans {
    pub resource: Option<Resource>,
    pub scope_spans: Vec<ScopeSpans>,
    pub schema_url: String,
}

pub fn encode_status(status: &Status) -> Vec<u8> {
    let mut out = Vec::new();
    write_string(&mut out, 2, &status.message);
    write_enum(&mut out, 3, status.code);
    out
}

pub fn decode_status(bytes: &[u8]) -> Result<Status, ProtoError> {
    let mut reader = Reader::new(bytes);
    let mut status = Status::default();
    while !reader.is_empty() {
        let (field, wire) = reader.read_tag()?;
        match (field, wire) {
            (2, WIRE_LEN) => status.message = reader.read_string()?,
            (3, WIRE_VARINT) => status.code = reader.read_varint()? as i32,
            (_, wire) => reader.skip(wire)?,
        }
    }
    Ok(status)
}

pub fn encode_span(span: &Span) -> Vec<u8> {
    let mut out = Vec::new();
    write_bytes(&mut out, 1, &span.trace_id);
    write_bytes(&mut out, 2, &span.span_id);
    write_string(&mut out, 3, &span.trace_state);
    write_bytes(&mut out, 4, &span.parent_span_id);
    write_fixed32(&mut out, 16, span.flags);
    write_string(&mut out, 5, &span.name);
    write_enum(&mut out, 6, span.kind);
    write_fixed64(&mut out, 7, span.start_time_unix_nano);
    write_fixed64(&mut out, 8, span.end_time_unix_nano);
    for pair in &span.attributes {
        write_message(&mut out, 9, &encode_key_value(pair));
    }
    write_uint(&mut out, 10, u64::from(span.dropped_attributes_count));
    if let Some(status) = &span.status {
        write_message(&mut out, 15, &encode_status(status));
    }
    out
}

pub fn decode_span(bytes: &[u8]) -> Result<Span, ProtoError> {
    let mut reader = Reader::new(bytes);
    let mut span = Span::default();
    while !reader.is_empty() {
        let (field, wire) = reader.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => span.trace_id = reader.read_len()?.to_vec(),
            (2, WIRE_LEN) => span.span_id = reader.read_len()?.to_vec(),
            (3, WIRE_LEN) => span.trace_state = reader.read_string()?,
            (4, WIRE_LEN) => span.parent_span_id = reader.read_len()?.to_vec(),
            (16, WIRE_FIXED32) => span.flags = reader.read_fixed32()?,
            (5, WIRE_LEN) => span.name = reader.read_string()?,
            (6, WIRE_VARINT) => span.kind = reader.read_varint()? as i32,
            (7, WIRE_FIXED64) => span.start_time_unix_nano = reader.read_fixed64()?,
            (8, WIRE_FIXED64) => span.end_time_unix_nano = reader.read_fixed64()?,
            (9, WIRE_LEN) => span.attributes.push(decode_key_value(reader.read_len()?)?),
            (10, WIRE_VARINT) => {
                span.dropped_attributes_count =
                    u32::try_from(reader.read_varint()?).unwrap_or(u32::MAX);
            }
            (15, WIRE_LEN) => span.status = Some(decode_status(reader.read_len()?)?),
            (_, wire) => reader.skip(wire)?,
        }
    }
    Ok(span)
}

pub fn encode_scope_spans(scope_spans: &ScopeSpans) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(scope) = &scope_spans.scope {
        write_message(&mut out, 1, &encode_instrumentation_scope(scope));
    }
    for span in &scope_spans.spans {
        write_message(&mut out, 2, &encode_span(span));
    }
    write_string(&mut out, 3, &scope_spans.schema_url);
    out
}

pub fn decode_scope_spans(bytes: &[u8]) -> Result<ScopeSpans, ProtoError> {
    let mut reader = Reader::new(bytes);
    let mut scope_spans = ScopeSpans::default();
    while !reader.is_empty() {
        let (field, wire) = reader.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => {
                scope_spans.scope = Some(decode_instrumentation_scope(reader.read_len()?)?);
            }
            (2, WIRE_LEN) => scope_spans.spans.push(decode_span(reader.read_len()?)?),
            (3, WIRE_LEN) => scope_spans.schema_url = reader.read_string()?,
            (_, wire) => reader.skip(wire)?,
        }
    }
    Ok(scope_spans)
}

pub fn encode_resource_spans(resource_spans: &ResourceSpans) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(resource) = &resource_spans.resource {
        write_message(&mut out, 1, &encode_resource_message(resource));
    }
    for scope_spans in &resource_spans.scope_spans {
        write_message(&mut out, 2, &encode_scope_spans(scope_spans));
    }
    write_string(&mut out, 3, &resource_spans.schema_url);
    out
}

pub fn decode_resource_spans(bytes: &[u8]) -> Result<ResourceSpans, ProtoError> {
    let mut reader = Reader::new(bytes);
    let mut resource_spans = ResourceSpans::default();
    while !reader.is_empty() {
        let (field, wire) = reader.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => {
                resource_spans.resource = Some(decode_resource_message(reader.read_len()?)?);
            }
            (2, WIRE_LEN) => resource_spans
                .scope_spans
                .push(decode_scope_spans(reader.read_len()?)?),
            (3, WIRE_LEN) => resource_spans.schema_url = reader.read_string()?,
            (_, wire) => reader.skip(wire)?,
        }
    }
    Ok(resource_spans)
}
