//! Port of the `google.protobuf.Value` codec senpi uses from
//! `@bufbuild/protobuf/wkt` (`ValueSchema`) for Cursor's `bytes`-typed
//! JSON-value fields.
//!
//! `agent.proto` carries these fields as `bytes` (protobuf-es serializes the
//! well-known type before putting it on the wire), so the Rust port needs the
//! same schema locally to encode and decode identically.

use prost::Message;
use serde_json::{Map, Value};

/// `google.protobuf.NullValue`.
pub const NULL_VALUE: i32 = 0;

/// `google.protobuf.Value`.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PbValue {
    #[prost(oneof = "pb_value::Kind", tags = "1, 2, 3, 4, 5, 6")]
    pub kind: Option<pb_value::Kind>,
}

pub mod pb_value {
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum Kind {
        #[prost(int32, tag = "1")]
        NullValue(i32),
        #[prost(double, tag = "2")]
        NumberValue(f64),
        #[prost(string, tag = "3")]
        StringValue(::prost::alloc::string::String),
        #[prost(bool, tag = "4")]
        BoolValue(bool),
        #[prost(message, tag = "5")]
        StructValue(super::PbStruct),
        #[prost(message, tag = "6")]
        ListValue(super::PbListValue),
    }
}

/// `google.protobuf.Struct`.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PbStruct {
    #[prost(map = "string, message", tag = "1")]
    pub fields: ::std::collections::HashMap<::prost::alloc::string::String, PbValue>,
}

/// `google.protobuf.ListValue`.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PbListValue {
    #[prost(message, repeated, tag = "1")]
    pub values: ::prost::alloc::vec::Vec<PbValue>,
}

/// JS `JSON.stringify` prints an integral double without a fraction, and `NaN`/`Infinity` as `null`.
fn js_number(number: f64) -> Value {
    if number.is_finite() && number.fract() == 0.0 && number.abs() < 9_007_199_254_740_992.0 {
        return Value::Number(serde_json::Number::from(number as i64));
    }
    serde_json::Number::from_f64(number).map(Value::Number).unwrap_or(Value::Null)
}

/// `fromJson(ValueSchema, value)`.
pub fn value_from_json(value: &Value) -> PbValue {
    let kind = match value {
        Value::Null => pb_value::Kind::NullValue(NULL_VALUE),
        Value::Bool(flag) => pb_value::Kind::BoolValue(*flag),
        Value::Number(number) => pb_value::Kind::NumberValue(number.as_f64().unwrap_or(f64::NAN)),
        Value::String(text) => pb_value::Kind::StringValue(text.clone()),
        Value::Array(items) => pb_value::Kind::ListValue(PbListValue {
            values: items.iter().map(value_from_json).collect(),
        }),
        Value::Object(entries) => pb_value::Kind::StructValue(PbStruct {
            fields: entries
                .iter()
                .map(|(key, item)| (key.clone(), value_from_json(item)))
                .collect(),
        }),
    };
    PbValue { kind: Some(kind) }
}

/// `toJson(ValueSchema, value)`.
pub fn value_to_json(value: &PbValue) -> Value {
    match &value.kind {
        None | Some(pb_value::Kind::NullValue(_)) => Value::Null,
        Some(pb_value::Kind::NumberValue(number)) => js_number(*number),
        Some(pb_value::Kind::StringValue(text)) => Value::String(text.clone()),
        Some(pb_value::Kind::BoolValue(flag)) => Value::Bool(*flag),
        Some(pb_value::Kind::ListValue(list)) => Value::Array(list.values.iter().map(value_to_json).collect()),
        Some(pb_value::Kind::StructValue(structure)) => {
            let mut entries = Map::new();
            for (key, item) in &structure.fields {
                entries.insert(key.clone(), value_to_json(item));
            }
            Value::Object(entries)
        }
    }
}

/// `toBinary(ValueSchema, fromJson(ValueSchema, value))`.
pub fn value_to_bytes(value: &Value) -> Vec<u8> {
    value_from_json(value).encode_to_vec()
}

/// `toJson(ValueSchema, fromBinary(ValueSchema, bytes))`.
pub fn value_from_bytes(bytes: &[u8]) -> Result<Value, prost::DecodeError> {
    Ok(value_to_json(&PbValue::decode(bytes)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_every_json_kind() {
        for value in [
            Value::Null,
            json!(true),
            json!(1.5),
            json!("text"),
            json!([1, "two", null, { "k": false }]),
            json!({ "a": 1, "b": { "c": [null] }, "d": "" }),
        ] {
            let bytes = value_to_bytes(&value);
            assert_eq!(value_from_bytes(&bytes).expect("decodes"), value, "value {value}");
        }
    }

    #[test]
    fn encodes_scalars_as_the_well_known_type() {
        assert_eq!(value_to_bytes(&Value::Null), vec![0x08, 0x00]);
        assert_eq!(value_to_bytes(&json!("hi")), vec![0x1a, 0x02, b'h', b'i']);
        assert_eq!(value_to_bytes(&json!(true)), vec![0x20, 0x01]);
        assert_eq!(value_to_bytes(&json!(1.0)).len(), 9);
        assert_eq!(value_to_bytes(&json!(1.0))[0], 0x11);
        assert_eq!(value_to_bytes(&json!({})), vec![0x2a, 0x00]);
        assert_eq!(value_to_bytes(&json!([])), vec![0x32, 0x00]);
    }

    #[test]
    fn rejects_garbage_bytes() {
        assert!(value_from_bytes(&[0x1a]).is_err());
    }
}
