//! Port of `chord/src/json.ts`.
use crate::types::{JsString, JsonValue};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default)]
pub struct CopyJsonOptions {
    pub omit_undefined_properties: bool,
}

/// Rust JSON values cannot contain undefined, accessors, holes, cycles, symbols,
/// or non-finite numbers. Deserialize into `serde_json::Value` at the boundary.
/// Optional object fields must use `skip_serializing_if = "Option::is_none"` to
/// represent JavaScript's `omitUndefinedProperties` rather than JSON null.
pub fn copy_json(value: &JsonValue, _options: CopyJsonOptions) -> JsonValue {
    match value {
        JsonValue::Null => JsonValue::Null,
        JsonValue::Bool(value) => JsonValue::Bool(*value),
        JsonValue::Number(value) => JsonValue::Number(value.clone()),
        JsonValue::String(value) => JsonValue::String(value.clone()),
        JsonValue::Array(values) => {
            JsonValue::array(values.iter().map(|v| copy_json(v, _options)).collect())
        }
        JsonValue::Object(values) => JsonValue::object(
            values
                .iter()
                .map(|(k, v)| (k.clone(), copy_json(v, _options)))
                .collect(),
        ),
    }
}

/// Every inhabitant of this type is strict JSON by construction.
pub const fn is_json_value(_value: &JsonValue) -> bool {
    true
}

impl From<Value> for JsonValue {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(v) => Self::Bool(v),
            Value::Number(v) => Self::Number(v),
            Value::String(v) => Self::String(v.into()),
            Value::Array(v) => Self::array(v.into_iter().map(Self::from).collect()),
            Value::Object(v) => {
                Self::object(v.into_iter().map(|(k, v)| (k.into(), Self::from(v))).collect())
            }
        }
    }
}

impl TryFrom<&JsonValue> for Value {
    type Error = serde_json::Error;
    fn try_from(value: &JsonValue) -> Result<Self, Self::Error> {
        serde_json::from_str(&serde_json::to_string(value)?)
    }
}

pub(crate) fn parse_string(text: &str) -> Result<JsString, serde_json::Error> {
    let raw: Box<serde_json::value::RawValue> = serde_json::from_str(text)?;
    let text = raw.get();
    if !text.starts_with('"') {
        return serde_json::from_str::<String>(text).map(JsString::from);
    }
    let mut units = Vec::new();
    let mut chars = text[1..text.len() - 1].chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            units.extend(ch.encode_utf16(&mut [0; 2]).iter().copied());
            continue;
        }
        match chars.next() {
            Some('u') => {
                let mut unit = 0u16;
                for digit in chars.by_ref().take(4) {
                    let number = digit.to_digit(16).ok_or_else(|| {
                        <serde_json::Error as serde::de::Error>::custom("invalid Unicode escape")
                    })?;
                    unit = unit * 16 + u16::try_from(number).map_err(serde::de::Error::custom)?;
                }
                units.push(unit);
            }
            Some('"') => units.push(34),
            Some('\\') => units.push(92),
            Some('/') => units.push(47),
            Some('b') => units.push(8),
            Some('f') => units.push(12),
            Some('n') => units.push(10),
            Some('r') => units.push(13),
            Some('t') => units.push(9),
            _ => return Err(serde::de::Error::custom("invalid string escape")),
        }
    }
    Ok(JsString::from_utf16(units))
}

pub fn parse(text: &str) -> Result<JsonValue, serde_json::Error> {
    let raw: Box<serde_json::value::RawValue> = serde_json::from_str(text)?;
    let text = raw.get();
    match text.as_bytes().first() {
        Some(b'"') => parse_string(text).map(JsonValue::String),
        Some(b'[') => {
            let children: Vec<Box<serde_json::value::RawValue>> = serde_json::from_str(text)?;
            Ok(JsonValue::array(
                children
                    .iter()
                    .map(|v| parse(v.get()))
                    .collect::<Result<_, _>>()?,
            ))
        }
        Some(b'{') => parse_object(text),
        _ => serde_json::from_str::<Value>(text).map(JsonValue::from),
    }
}

// The enclosing RawValue has already checked JSON syntax. Scan token boundaries
// here because serde's map-key visitor requires Unicode scalar strings.
fn parse_object(text: &str) -> Result<JsonValue, serde_json::Error> {
    let bytes = text.as_bytes();
    let mut at = 1;
    let mut entries = Vec::new();
    loop {
        while bytes[at].is_ascii_whitespace() || bytes[at] == b',' { at += 1; }
        if bytes[at] == b'}' { break; }
        let start = at;
        at += 1;
        while bytes[at] != b'"' {
            if bytes[at] == b'\\' { at += 1; }
            at += 1;
        }
        at += 1;
        let key = parse_string(&text[start..at])?;
        while bytes[at].is_ascii_whitespace() || bytes[at] == b':' { at += 1; }
        let start = at;
        let mut depth = 0usize;
        let mut quoted = false;
        loop {
            let byte = bytes[at];
            if quoted {
                if byte == b'\\' { at += 1; }
                else if byte == b'"' { quoted = false; }
            } else {
                match byte {
                    b'"' => quoted = true,
                    b'[' | b'{' => depth += 1,
                    b']' | b'}' if depth != 0 => depth -= 1,
                    b',' | b'}' if depth == 0 => break,
                    _ => {}
                }
            }
            at += 1;
        }
        entries.push((key, parse(&text[start..at])?));
    }
    Ok(JsonValue::object(entries))
}

pub(crate) fn field<T: serde::de::DeserializeOwned>(
    value: &JsonValue,
    key: &str,
) -> Result<T, serde_json::Error> {
    serde_json::from_str(&serde_json::to_string(&value[key])?)
}

macro_rules! deserialize_tagged {
    ($name:ident, $tag:literal, {$($wire:literal => $variant:ident {$($field:ident),*}),* $(,)?}) => {
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let value = crate::types::JsonValue::deserialize(d)?;
                let object = value.as_object().ok_or_else(|| <D::Error as serde::de::Error>::custom("expected Chord object"))?;
                match value[$tag].as_str() {
                    $(Some($wire) => {
                        let allowed = &[$tag, $(stringify!($field)),*];
                        if object.iter().any(|(key, _)| !key.as_str().is_some_and(|key| allowed.contains(&key))) { return Err(serde::de::Error::custom("unknown Chord field")); }
                        Ok(Self::$variant { $($field: crate::json::field(&value, stringify!($field)).map_err(serde::de::Error::custom)?),* })
                    },)*
                    _ => Err(serde::de::Error::custom("invalid tagged Chord value")),
                }
            }
        }
    };
}
pub(crate) use deserialize_tagged;

impl JsonValue {
    pub fn get_key(&self, key: &JsString) -> Option<&Self> {
        match self {
            Self::Object(values) => values.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(values) => values.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_object(&self) -> Option<&crate::types::JsonObject> {
        match self {
            Self::Object(values) => Some(values),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&Vec<Self>> {
        match self {
            Self::Array(value) => Some(value),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => value.as_str(),
            _ => None,
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(value) => value.as_u64(),
            _ => None,
        }
    }
    pub fn is_number(&self) -> bool {
        matches!(self, Self::Number(_))
    }
}
impl std::ops::Index<usize> for JsonValue {
    type Output = Self;
    fn index(&self, index: usize) -> &Self {
        match self {
            Self::Array(values) => values.get(index).unwrap_or(&Self::Null),
            _ => &Self::Null,
        }
    }
}
impl std::ops::Index<&str> for JsonValue {
    type Output = Self;
    fn index(&self, key: &str) -> &Self {
        self.get(key).unwrap_or(&Self::Null)
    }
}
