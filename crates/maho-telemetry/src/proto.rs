//! A minimal protobuf writer for the OTLP messages this crate sends. Field numbers are the
//! interface facts of the public `opentelemetry-proto` definitions (common, resource, trace,
//! metrics, logs and their collector service requests); no generated code is used.

/// Protobuf wire types.
#[derive(Clone, Copy)]
enum Wire {
    Varint = 0,
    Fixed64 = 1,
    Len = 2,
}

/// One message being encoded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Message {
    buf: Vec<u8>,
}

fn put_varint(buf: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        buf.push((value as u8) | 0x80);
        value >>= 7;
    }
    buf.push(value as u8);
}

impl Message {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(&mut self, field: u32, wire: Wire) {
        put_varint(&mut self.buf, (u64::from(field) << 3) | wire as u64);
    }

    /// `uint64`/`int64`/`int32`/enum/`bool` as a varint (always written, even when zero).
    pub fn varint(&mut self, field: u32, value: u64) -> &mut Self {
        self.key(field, Wire::Varint);
        put_varint(&mut self.buf, value);
        self
    }

    /// Signed integer as a two's-complement varint (`int64`).
    pub fn int64(&mut self, field: u32, value: i64) -> &mut Self {
        // Two's-complement reinterpretation is the protobuf `int64` encoding.
        self.varint(field, value as u64)
    }

    /// `fixed64`/`sfixed64`.
    pub fn fixed64(&mut self, field: u32, value: u64) -> &mut Self {
        self.key(field, Wire::Fixed64);
        self.buf.extend_from_slice(&value.to_le_bytes());
        self
    }

    /// `double`.
    pub fn double(&mut self, field: u32, value: f64) -> &mut Self {
        self.fixed64(field, value.to_bits())
    }

    /// `bytes`.
    pub fn bytes(&mut self, field: u32, value: &[u8]) -> &mut Self {
        self.key(field, Wire::Len);
        put_varint(&mut self.buf, value.len() as u64);
        self.buf.extend_from_slice(value);
        self
    }

    /// `string`.
    pub fn string(&mut self, field: u32, value: &str) -> &mut Self {
        self.bytes(field, value.as_bytes())
    }

    /// Embedded message.
    pub fn message(&mut self, field: u32, value: &Message) -> &mut Self {
        self.bytes(field, &value.buf)
    }

    /// Packed `repeated fixed64`.
    pub fn packed_fixed64(&mut self, field: u32, values: &[u64]) -> &mut Self {
        let packed: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.bytes(field, &packed)
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }
}

/// An attribute value (`opentelemetry.proto.common.v1.AnyValue`).
#[derive(Clone, Debug, PartialEq)]
pub enum AnyValue {
    String(String),
    Bool(bool),
    Int(i64),
    Double(f64),
    Array(Vec<AnyValue>),
}

impl AnyValue {
    pub fn encode(&self) -> Message {
        let mut message = Message::new();
        match self {
            Self::String(value) => message.string(1, value),
            Self::Bool(value) => message.varint(2, u64::from(*value)),
            Self::Int(value) => message.int64(3, *value),
            Self::Double(value) => message.double(4, *value),
            Self::Array(values) => {
                let mut array = Message::new();
                for value in values {
                    array.message(1, &value.encode());
                }
                message.message(5, &array)
            }
        };
        message
    }
}

impl From<&str> for AnyValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}
impl From<String> for AnyValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}
impl From<i64> for AnyValue {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}
impl From<u64> for AnyValue {
    fn from(value: u64) -> Self {
        Self::Int(i64::try_from(value).unwrap_or(i64::MAX))
    }
}
impl From<f64> for AnyValue {
    fn from(value: f64) -> Self {
        Self::Double(value)
    }
}
impl From<bool> for AnyValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

/// Attribute list (`repeated KeyValue`).
pub type Attributes = Vec<(String, AnyValue)>;

/// `KeyValue { key = 1; value = 2 }`.
pub fn key_value(key: &str, value: &AnyValue) -> Message {
    let mut message = Message::new();
    message.string(1, key).message(2, &value.encode());
    message
}

/// Writes `attributes` as `repeated KeyValue` at `field`.
pub fn put_attributes(message: &mut Message, field: u32, attributes: &Attributes) {
    for (key, value) in attributes {
        message.message(field, &key_value(key, value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_and_keys_follow_the_wire_format() {
        let mut message = Message::new();
        message.varint(1, 150);
        assert_eq!(message.as_bytes(), [0x08, 0x96, 0x01]);
        let mut message = Message::new();
        message.string(2, "testing");
        assert_eq!(message.as_bytes(), b"\x12\x07testing");
        let mut message = Message::new();
        message.int64(3, -1);
        assert_eq!(message.as_bytes().len(), 11);
        let mut message = Message::new();
        message.fixed64(7, 1);
        assert_eq!(message.as_bytes(), [0x39, 1, 0, 0, 0, 0, 0, 0, 0]);
        let mut message = Message::new();
        message.packed_fixed64(6, &[1, 2]);
        assert_eq!(message.as_bytes()[..2], [0x32, 16]);
    }

    #[test]
    fn any_value_and_key_value_nest() {
        let kv = key_value("k", &AnyValue::from("v"));
        assert_eq!(kv.as_bytes(), b"\x0a\x01k\x12\x03\x0a\x01v");
        let array = AnyValue::Array(vec![AnyValue::Bool(true)]).encode();
        assert_eq!(array.as_bytes(), [0x2a, 0x04, 0x0a, 0x02, 0x10, 0x01]);
        assert_eq!(AnyValue::from(u64::MAX), AnyValue::Int(i64::MAX));
    }
}
