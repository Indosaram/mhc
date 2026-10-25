//! Rust port of `packages/telemetry-core/src/otlp-proto/`: the OTLP protobuf messages the
//! telemetry transport encodes and decodes, written directly against the protobuf wire format.
//!
//! The upstream package generates these from `.proto` files with `protoc-gen-es`. The Rust port
//! keeps the same messages, field numbers and proto3 default-elision rules, without a protobuf
//! code generator.

pub mod collector_logs;
pub mod collector_trace;
pub mod common;
pub mod logs;
pub mod resource;
pub mod trace;

use std::fmt;

pub const WIRE_VARINT: u8 = 0;
pub const WIRE_FIXED64: u8 = 1;
pub const WIRE_LEN: u8 = 2;
pub const WIRE_FIXED32: u8 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtoError {
    pub message: String,
}

impl ProtoError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ProtoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for ProtoError {}

pub fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

pub fn write_tag(out: &mut Vec<u8>, field: u32, wire: u8) {
    write_varint(out, (u64::from(field) << 3) | u64::from(wire));
}

/// Length-delimited field, including an empty payload (a oneof member keeps its presence).
pub fn write_len_field(out: &mut Vec<u8>, field: u32, payload: &[u8]) {
    write_tag(out, field, WIRE_LEN);
    write_varint(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

/// proto3: an empty string is the default and is not written.
pub fn write_string(out: &mut Vec<u8>, field: u32, value: &str) {
    if value.is_empty() {
        return;
    }
    write_len_field(out, field, value.as_bytes());
}

/// proto3: empty bytes are the default and are not written.
pub fn write_bytes(out: &mut Vec<u8>, field: u32, value: &[u8]) {
    if value.is_empty() {
        return;
    }
    write_len_field(out, field, value);
}

pub fn write_message(out: &mut Vec<u8>, field: u32, body: &[u8]) {
    write_len_field(out, field, body);
}

/// proto3: zero is the default and is not written.
pub fn write_uint(out: &mut Vec<u8>, field: u32, value: u64) {
    if value == 0 {
        return;
    }
    write_tag(out, field, WIRE_VARINT);
    write_varint(out, value);
}

pub fn write_int64(out: &mut Vec<u8>, field: u32, value: i64) {
    write_uint(out, field, value as u64);
}

pub fn write_enum(out: &mut Vec<u8>, field: u32, value: i32) {
    if value == 0 {
        return;
    }
    write_tag(out, field, WIRE_VARINT);
    write_varint(out, value as u64);
}

pub fn write_bool(out: &mut Vec<u8>, field: u32, value: bool) {
    if !value {
        return;
    }
    write_tag(out, field, WIRE_VARINT);
    write_varint(out, 1);
}

pub fn write_fixed64(out: &mut Vec<u8>, field: u32, value: u64) {
    if value == 0 {
        return;
    }
    write_tag(out, field, WIRE_FIXED64);
    out.extend_from_slice(&value.to_le_bytes());
}

pub fn write_fixed32(out: &mut Vec<u8>, field: u32, value: u32) {
    if value == 0 {
        return;
    }
    write_tag(out, field, WIRE_FIXED32);
    out.extend_from_slice(&value.to_le_bytes());
}

pub fn write_double(out: &mut Vec<u8>, field: u32, value: f64) {
    if value == 0.0 {
        return;
    }
    write_tag(out, field, WIRE_FIXED64);
    out.extend_from_slice(&value.to_bits().to_le_bytes());
}

/// A cursor over one encoded message.
pub struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.at >= self.bytes.len()
    }

    pub fn read_varint(&mut self) -> Result<u64, ProtoError> {
        let mut value = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = *self
                .bytes
                .get(self.at)
                .ok_or_else(|| ProtoError::new("truncated varint"))?;
            self.at += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
            if shift >= 64 {
                return Err(ProtoError::new("varint is too long"));
            }
        }
    }

    pub fn read_tag(&mut self) -> Result<(u32, u8), ProtoError> {
        let tag = self.read_varint()?;
        let field = u32::try_from(tag >> 3).map_err(|_| ProtoError::new("field number too large"))?;
        Ok((field, (tag & 0x07) as u8))
    }

    pub fn read_len(&mut self) -> Result<&'a [u8], ProtoError> {
        let length = usize::try_from(self.read_varint()?)
            .map_err(|_| ProtoError::new("length is too large"))?;
        let end = self
            .at
            .checked_add(length)
            .ok_or_else(|| ProtoError::new("length overflow"))?;
        if end > self.bytes.len() {
            return Err(ProtoError::new("truncated length-delimited field"));
        }
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    pub fn read_string(&mut self) -> Result<String, ProtoError> {
        let bytes = self.read_len()?;
        String::from_utf8(bytes.to_vec()).map_err(|_| ProtoError::new("invalid utf-8"))
    }

    pub fn read_fixed64(&mut self) -> Result<u64, ProtoError> {
        let end = self.at + 8;
        let slice = self
            .bytes
            .get(self.at..end)
            .ok_or_else(|| ProtoError::new("truncated fixed64"))?;
        self.at = end;
        let mut raw = [0u8; 8];
        raw.copy_from_slice(slice);
        Ok(u64::from_le_bytes(raw))
    }

    pub fn read_fixed32(&mut self) -> Result<u32, ProtoError> {
        let end = self.at + 4;
        let slice = self
            .bytes
            .get(self.at..end)
            .ok_or_else(|| ProtoError::new("truncated fixed32"))?;
        self.at = end;
        let mut raw = [0u8; 4];
        raw.copy_from_slice(slice);
        Ok(u32::from_le_bytes(raw))
    }

    pub fn skip(&mut self, wire: u8) -> Result<(), ProtoError> {
        match wire {
            WIRE_VARINT => {
                self.read_varint()?;
            }
            WIRE_FIXED64 => {
                self.read_fixed64()?;
            }
            WIRE_LEN => {
                self.read_len()?;
            }
            WIRE_FIXED32 => {
                self.read_fixed32()?;
            }
            _ => return Err(ProtoError::new("unsupported wire type")),
        }
        Ok(())
    }
}
