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
