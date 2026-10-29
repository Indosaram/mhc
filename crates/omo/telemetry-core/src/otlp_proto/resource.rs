//! Rust port of `otlp-proto/opentelemetry/proto/resource/v1/resource_pb.ts`.

use super::ProtoError;
use super::Reader;
use super::WIRE_LEN;
use super::WIRE_VARINT;
use super::common::KeyValue;
use super::common::decode_key_value;
use super::common::encode_key_value;
use super::write_message;
use super::write_uint;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Resource {
    pub attributes: Vec<KeyValue>,
    pub dropped_attributes_count: u32,
}

pub fn encode_resource_message(resource: &Resource) -> Vec<u8> {
    let mut out = Vec::new();
    for pair in &resource.attributes {
        write_message(&mut out, 1, &encode_key_value(pair));
    }
    write_uint(&mut out, 2, u64::from(resource.dropped_attributes_count));
    out
}

pub fn decode_resource_message(bytes: &[u8]) -> Result<Resource, ProtoError> {
    let mut reader = Reader::new(bytes);
    let mut resource = Resource::default();
    while !reader.is_empty() {
        let (field, wire) = reader.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => resource.attributes.push(decode_key_value(reader.read_len()?)?),
            (2, WIRE_VARINT) => {
                resource.dropped_attributes_count =
                    u32::try_from(reader.read_varint()?).unwrap_or(u32::MAX);
            }
            (_, wire) => reader.skip(wire)?,
        }
    }
    Ok(resource)
}
