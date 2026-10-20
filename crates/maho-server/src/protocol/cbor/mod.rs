pub mod decoder;
pub mod encoder;
pub mod options;

pub use decoder::decode_cbor;
pub use encoder::encode_cbor;
pub use options::{CborError, CborOptions};

/// The wire subset also supports byte strings, which are not JSON protocol payloads.
#[derive(Clone, Debug, PartialEq)]
pub enum CborValue {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<CborValue>),
    Map(Vec<(String, CborValue)>),
}

impl CborValue {
    pub fn from_chord(value: &maho_chord::types::JsonValue) -> Result<Self, CborError> {
        use maho_chord::types::JsonValue;
        Ok(match value {
            JsonValue::Null => Self::Null,
            JsonValue::Bool(value) => Self::Bool(*value),
            JsonValue::Number(value) => {
                if let Some(number) = value.as_i64() {
                    Self::Integer(number)
                } else if let Some(number) = value.as_u64() {
                    Self::Integer(i64::try_from(number).map_err(|_| CborError("CBOR integers must be safe JavaScript integers".into()))?)
                } else {
                let number = value.as_f64().ok_or_else(|| CborError("CBOR numbers must be finite".into()))?;
                Self::Float(number)
                }
            }
            JsonValue::String(value) => Self::Text(value.as_str().ok_or_else(|| CborError("CBOR text strings must contain valid Unicode scalar values".into()))?.into()),
            JsonValue::Array(values) => Self::Array(values.iter().map(Self::from_chord).collect::<Result<_, _>>()?),
            JsonValue::Object(values) => Self::Map(values.iter().map(|(key, value)| {
                let key = key.as_str().ok_or_else(|| CborError("CBOR text strings must contain valid Unicode scalar values".into()))?;
                Ok((key.into(), Self::from_chord(value)?))
            }).collect::<Result<_, CborError>>()?),
        })
    }

    pub fn into_chord(self) -> Result<maho_chord::types::JsonValue, CborError> {
        use maho_chord::types::JsonValue;
        Ok(match self {
            Self::Null => JsonValue::Null,
            Self::Bool(value) => JsonValue::Bool(value),
            Self::Integer(value) => JsonValue::Number(value.into()),
            Self::Float(value) => JsonValue::Number(serde_json::Number::from_f64(value).ok_or_else(|| CborError("Decoded CBOR number must be finite".into()))?),
            Self::Text(value) => JsonValue::String(value.into()),
            Self::Bytes(_) => return Err(CborError("Byte strings are not JSON values".into())),
            Self::Array(values) => JsonValue::array(values.into_iter().map(Self::into_chord).collect::<Result<_, _>>()?),
            Self::Map(values) => JsonValue::object(values.into_iter().map(|(key, value)| Ok((key.into(), value.into_chord()?))).collect::<Result<_, CborError>>()?),
        })
    }

    pub fn from_json(value: &serde_json::Value) -> Result<Self, CborError> {
        use serde_json::Value;
        Ok(match value {
            Value::Null => Self::Null,
            Value::Bool(v) => Self::Bool(*v),
            Value::Number(v) => {
                if let Some(n) = v.as_i64() {
                    Self::Integer(n)
                } else if let Some(n) = v.as_u64() {
                    Self::Integer(i64::try_from(n).map_err(|_| {
                        CborError("CBOR integers must be safe JavaScript integers".into())
                    })?)
                } else {
                    Self::Float(
                        v.as_f64()
                            .ok_or_else(|| CborError("CBOR numbers must be finite".into()))?,
                    )
                }
            }
            Value::String(v) => Self::Text(v.clone()),
            Value::Array(v) => {
                Self::Array(v.iter().map(Self::from_json).collect::<Result<_, _>>()?)
            }
            Value::Object(v) => Self::Map(
                v.iter()
                    .map(|(k, v)| Ok((k.clone(), Self::from_json(v)?)))
                    .collect::<Result<_, CborError>>()?,
            ),
        })
    }

    pub fn into_json(self) -> Result<serde_json::Value, CborError> {
        use serde_json::Value;
        Ok(match self {
            Self::Null => Value::Null,
            Self::Bool(v) => Value::Bool(v),
            Self::Integer(v) => Value::Number(v.into()),
            Self::Float(v) => Value::Number(
                serde_json::Number::from_f64(v)
                    .ok_or_else(|| CborError("Decoded CBOR number must be finite".into()))?,
            ),
            Self::Bytes(_) => return Err(CborError("Byte strings are not JSON values".into())),
            Self::Text(v) => Value::String(v),
            Self::Array(v) => Value::Array(
                v.into_iter()
                    .map(Self::into_json)
                    .collect::<Result<_, _>>()?,
            ),
            Self::Map(v) => Value::Object(
                v.into_iter()
                    .map(|(k, v)| Ok((k, v.into_json()?)))
                    .collect::<Result<_, CborError>>()?,
            ),
        })
    }
}

#[cfg(test)]
mod chord_tests {
    use super::*;
    use maho_chord::types::JsonValue;

    #[test]
    fn rejects_isolated_surrogates_at_scalar_cbor_boundary() {
        for text in [r#""\ud800""#, r#"{"\udfff":1}"#] {
            let value: JsonValue = serde_json::from_str(text).expect("lossless JSON");
            assert_eq!(CborValue::from_chord(&value).expect_err("not scalar UTF8").0, "CBOR text strings must contain valid Unicode scalar values");
            assert_eq!(serde_json::to_string(&value).expect("unchanged source"), text);
        }
    }

    #[test]
    fn native_cbor_roundtrip_preserves_root_arrays_and_negative_zero() {
        let value: JsonValue = serde_json::from_str(r#"[null,true,"\ud83d\udca0",{"n":-0.0},[1,2,3]]"#).expect("fixture");
        let bytes = encode_cbor(&CborValue::from_chord(&value).expect("scalar values"), CborOptions::default()).expect("encode");
        let decoded = decode_cbor(&bytes, CborOptions::default()).expect("decode").into_chord().expect("native JSON");
        assert_eq!(decoded, value);
        let JsonValue::Number(number) = &decoded[3]["n"] else { panic!("number"); };
        assert_eq!(number.as_f64().expect("finite number").to_bits(), (-0.0f64).to_bits());
    }

    #[test]
    fn rejects_unsafe_service_numbers_only_when_encoding_cbor() {
        let value: JsonValue = serde_json::from_str("18446744073709551616").expect("native number");
        let cbor = CborValue::from_chord(&value).expect("native number boundary");
        assert!(encode_cbor(&cbor, CborOptions::default()).is_err());
    }
}
