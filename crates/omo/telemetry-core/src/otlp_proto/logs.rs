//! Rust port of `otlp-proto/opentelemetry/proto/logs/v1/logs_pb.ts`.

use super::ProtoError;
use super::Reader;
use super::WIRE_FIXED32;
use super::WIRE_FIXED64;
use super::WIRE_LEN;
use super::WIRE_VARINT;
use super::common::AnyValue;
use super::common::InstrumentationScope;
use super::common::KeyValue;
use super::common::decode_any_value;
use super::common::decode_instrumentation_scope;
use super::common::decode_key_value;
use super::common::encode_any_value;
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

pub const SEVERITY_NUMBER_UNSPECIFIED: i32 = 0;
pub const SEVERITY_NUMBER_FATAL4: i32 = 24;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogRecord {
    pub time_unix_nano: u64,
    pub observed_time_unix_nano: u64,
    pub severity_number: i32,
    pub severity_text: String,
    pub body: Option<AnyValue>,
    pub attributes: Vec<KeyValue>,
    pub dropped_attributes_count: u32,
    pub flags: u32,
    pub trace_id: Vec<u8>,
    pub span_id: Vec<u8>,
    pub event_name: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScopeLogs {
    pub scope: Option<InstrumentationScope>,
    pub log_records: Vec<LogRecord>,
    pub schema_url: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResourceLogs {
    pub resource: Option<Resource>,
    pub scope_logs: Vec<ScopeLogs>,
    pub schema_url: String,
}

/// A severity outside the enum's range reads as `UNSPECIFIED`, exactly like the TS lookup.
pub fn severity(value: i32) -> i32 {
    if (SEVERITY_NUMBER_UNSPECIFIED..=SEVERITY_NUMBER_FATAL4).contains(&value) {
        value
    } else {
        SEVERITY_NUMBER_UNSPECIFIED
    }
}

pub fn encode_log_record(record: &LogRecord) -> Vec<u8> {
    let mut out = Vec::new();
    write_fixed64(&mut out, 1, record.time_unix_nano);
    write_fixed64(&mut out, 11, record.observed_time_unix_nano);
    write_enum(&mut out, 2, record.severity_number);
    write_string(&mut out, 3, &record.severity_text);
    if let Some(body) = &record.body {
        write_message(&mut out, 5, &encode_any_value(body));
    }
    for pair in &record.attributes {
        write_message(&mut out, 6, &encode_key_value(pair));
    }
    write_uint(&mut out, 7, u64::from(record.dropped_attributes_count));
    write_fixed32(&mut out, 8, record.flags);
    write_bytes(&mut out, 9, &record.trace_id);
    write_bytes(&mut out, 10, &record.span_id);
    write_string(&mut out, 12, &record.event_name);
    out
}

pub fn decode_log_record(bytes: &[u8]) -> Result<LogRecord, ProtoError> {
    let mut reader = Reader::new(bytes);
    let mut record = LogRecord::default();
    while !reader.is_empty() {
        let (field, wire) = reader.read_tag()?;
        match (field, wire) {
            (1, WIRE_FIXED64) => record.time_unix_nano = reader.read_fixed64()?,
            (11, WIRE_FIXED64) => record.observed_time_unix_nano = reader.read_fixed64()?,
            (2, WIRE_VARINT) => record.severity_number = reader.read_varint()? as i32,
            (3, WIRE_LEN) => record.severity_text = reader.read_string()?,
            (5, WIRE_LEN) => record.body = Some(decode_any_value(reader.read_len()?)?),
            (6, WIRE_LEN) => record.attributes.push(decode_key_value(reader.read_len()?)?),
            (7, WIRE_VARINT) => {
                record.dropped_attributes_count =
                    u32::try_from(reader.read_varint()?).unwrap_or(u32::MAX);
            }
            (8, WIRE_FIXED32) => record.flags = reader.read_fixed32()?,
            (9, WIRE_LEN) => record.trace_id = reader.read_len()?.to_vec(),
            (10, WIRE_LEN) => record.span_id = reader.read_len()?.to_vec(),
            (12, WIRE_LEN) => record.event_name = reader.read_string()?,
            (_, wire) => reader.skip(wire)?,
        }
    }
    Ok(record)
}

pub fn encode_scope_logs(scope_logs: &ScopeLogs) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(scope) = &scope_logs.scope {
        write_message(&mut out, 1, &encode_instrumentation_scope(scope));
    }
    for record in &scope_logs.log_records {
        write_message(&mut out, 2, &encode_log_record(record));
    }
    write_string(&mut out, 3, &scope_logs.schema_url);
    out
}

pub fn decode_scope_logs(bytes: &[u8]) -> Result<ScopeLogs, ProtoError> {
    let mut reader = Reader::new(bytes);
    let mut scope_logs = ScopeLogs::default();
    while !reader.is_empty() {
        let (field, wire) = reader.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => {
                scope_logs.scope = Some(decode_instrumentation_scope(reader.read_len()?)?);
            }
            (2, WIRE_LEN) => scope_logs.log_records.push(decode_log_record(reader.read_len()?)?),
            (3, WIRE_LEN) => scope_logs.schema_url = reader.read_string()?,
            (_, wire) => reader.skip(wire)?,
        }
    }
    Ok(scope_logs)
}

pub fn encode_resource_logs(resource_logs: &ResourceLogs) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(resource) = &resource_logs.resource {
        write_message(&mut out, 1, &encode_resource_message(resource));
    }
    for scope_logs in &resource_logs.scope_logs {
        write_message(&mut out, 2, &encode_scope_logs(scope_logs));
    }
    write_string(&mut out, 3, &resource_logs.schema_url);
    out
}

pub fn decode_resource_logs(bytes: &[u8]) -> Result<ResourceLogs, ProtoError> {
    let mut reader = Reader::new(bytes);
    let mut resource_logs = ResourceLogs::default();
    while !reader.is_empty() {
        let (field, wire) = reader.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => {
                resource_logs.resource = Some(decode_resource_message(reader.read_len()?)?);
            }
            (2, WIRE_LEN) => resource_logs.scope_logs.push(decode_scope_logs(reader.read_len()?)?),
            (3, WIRE_LEN) => resource_logs.schema_url = reader.read_string()?,
            (_, wire) => reader.skip(wire)?,
        }
    }
    Ok(resource_logs)
}
