//! Rust port of `otlp-proto/opentelemetry/proto/common/v1/common_pb.ts`.

use super::ProtoError;
use super::Reader;
use super::WIRE_FIXED64;
use super::WIRE_LEN;
use super::WIRE_VARINT;
use super::write_len_field;
use super::write_message;
use super::write_string;
use super::write_tag;
use super::write_uint;
use super::write_varint;

/// `AnyValue`: a oneof, so a member keeps its presence even when it carries a default value.
#[derive(Debug, Clone, PartialEq)]
pub enum AnyValue {
    Empty,
    String(String),
    Bool(bool),
    Int(i64),
    Double(f64),
    Array(Vec<AnyValue>),
    KvList(Vec<KeyValue>),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeyValue {
    pub key: String,
    pub value: AnyValue,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct InstrumentationScope {
    pub name: String,
    pub version: String,
    pub attributes: Vec<KeyValue>,
    pub dropped_attributes_count: u32,
}

pub fn encode_any_value(value: &AnyValue) -> Vec<u8> {
    let mut out = Vec::new();
    match value {
        AnyValue::Empty => {}
        AnyValue::String(text) => write_len_field(&mut out, 1, text.as_bytes()),
        AnyValue::Bool(flag) => {
            write_tag(&mut out, 2, WIRE_VARINT);
            write_varint(&mut out, u64::from(*flag));
        }
        AnyValue::Int(number) => {
            write_tag(&mut out, 3, WIRE_VARINT);
            write_varint(&mut out, *number as u64);
        }
        AnyValue::Double(number) => {
            write_tag(&mut out, 4, WIRE_FIXED64);
            out.extend_from_slice(&number.to_bits().to_le_bytes());
        }
        AnyValue::Array(values) => {
            let mut body = Vec::new();
            for value in values {
                write_message(&mut body, 1, &encode_any_value(value));
            }
            write_message(&mut out, 5, &body);
        }
        AnyValue::KvList(pairs) => {
            let mut body = Vec::new();
            for pair in pairs {
                write_message(&mut body, 1, &encode_key_value(pair));
            }
            write_message(&mut out, 6, &body);
        }
        AnyValue::Bytes(bytes) => write_len_field(&mut out, 7, bytes),
    }
    out
}

pub fn decode_any_value(bytes: &[u8]) -> Result<AnyValue, ProtoError> {
    let mut reader = Reader::new(bytes);
    let mut value = AnyValue::Empty;
    while !reader.is_empty() {
        let (field, wire) = reader.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => value = AnyValue::String(reader.read_string()?),
            (2, WIRE_VARINT) => value = AnyValue::Bool(reader.read_varint()? != 0),
            (3, WIRE_VARINT) => value = AnyValue::Int(reader.read_varint()? as i64),
            (4, WIRE_FIXED64) => value = AnyValue::Double(f64::from_bits(reader.read_fixed64()?)),
            (5, WIRE_LEN) => {
                let body = reader.read_len()?;
                let mut nested = Reader::new(body);
                let mut values = Vec::new();
                while !nested.is_empty() {
                    let (nested_field, nested_wire) = nested.read_tag()?;
                    if nested_field == 1 && nested_wire == WIRE_LEN {
                        values.push(decode_any_value(nested.read_len()?)?);
                    } else {
                        nested.skip(nested_wire)?;
                    }
                }
                value = AnyValue::Array(values);
            }
            (6, WIRE_LEN) => {
                let body = reader.read_len()?;
                let mut nested = Reader::new(body);
                let mut pairs = Vec::new();
                while !nested.is_empty() {
                    let (nested_field, nested_wire) = nested.read_tag()?;
                    if nested_field == 1 && nested_wire == WIRE_LEN {
                        pairs.push(decode_key_value(nested.read_len()?)?);
                    } else {
                        nested.skip(nested_wire)?;
                    }
                }
                value = AnyValue::KvList(pairs);
            }
            (7, WIRE_LEN) => value = AnyValue::Bytes(reader.read_len()?.to_vec()),
            (_, wire) => reader.skip(wire)?,
        }
    }
    Ok(value)
}

pub fn encode_key_value(pair: &KeyValue) -> Vec<u8> {
    let mut out = Vec::new();
    write_string(&mut out, 1, &pair.key);
    if pair.value != AnyValue::Empty {
        write_message(&mut out, 2, &encode_any_value(&pair.value));
    }
    out
}

pub fn decode_key_value(bytes: &[u8]) -> Result<KeyValue, ProtoError> {
    let mut reader = Reader::new(bytes);
    let mut pair = KeyValue {
        key: String::new(),
        value: AnyValue::Empty,
    };
    while !reader.is_empty() {
        let (field, wire) = reader.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => pair.key = reader.read_string()?,
            (2, WIRE_LEN) => pair.value = decode_any_value(reader.read_len()?)?,
            (_, wire) => reader.skip(wire)?,
        }
    }
    Ok(pair)
}

pub fn encode_instrumentation_scope(scope: &InstrumentationScope) -> Vec<u8> {
    let mut out = Vec::new();
    write_string(&mut out, 1, &scope.name);
    write_string(&mut out, 2, &scope.version);
    for pair in &scope.attributes {
        write_message(&mut out, 3, &encode_key_value(pair));
    }
    write_uint(
        &mut out,
        4,
        u64::from(scope.dropped_attributes_count),
    );
    out
}

pub fn decode_instrumentation_scope(bytes: &[u8]) -> Result<InstrumentationScope, ProtoError> {
    let mut reader = Reader::new(bytes);
    let mut scope = InstrumentationScope::default();
    while !reader.is_empty() {
        let (field, wire) = reader.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => scope.name = reader.read_string()?,
            (2, WIRE_LEN) => scope.version = reader.read_string()?,
            (3, WIRE_LEN) => scope.attributes.push(decode_key_value(reader.read_len()?)?),
            (4, WIRE_VARINT) => {
                scope.dropped_attributes_count = u32::try_from(reader.read_varint()?).unwrap_or(u32::MAX);
            }
            (_, wire) => reader.skip(wire)?,
        }
    }
    Ok(scope)
}
