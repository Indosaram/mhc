//! JavaScript strings are sequences of UTF-16 code units, not Unicode scalars.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
enum Repr {
    Utf8(String),
    Utf16(Vec<u16>),
}

#[derive(Clone, Debug)]
pub struct JsString(Repr);

impl JsString {
    pub fn from_utf16(units: Vec<u16>) -> Self {
        match String::from_utf16(&units) {
            Ok(value) => value.into(),
            Err(_) => Self(Repr::Utf16(units)),
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match &self.0 {
            Repr::Utf8(value) => Some(value),
            Repr::Utf16(_) => None,
        }
    }
    pub fn units(&self) -> Vec<u16> {
        match &self.0 {
            Repr::Utf8(value) => value.encode_utf16().collect(),
            Repr::Utf16(value) => value.clone(),
        }
    }
    pub fn len(&self) -> usize {
        match &self.0 {
            Repr::Utf8(value) => value.encode_utf16().count(),
            Repr::Utf16(value) => value.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn slice(&self, start: usize) -> Self {
        Self::from_utf16(self.units().into_iter().skip(start).collect())
    }
    pub fn concat(&self, suffix: &Self) -> Self {
        if let (Some(left), Some(right)) = (self.as_str(), suffix.as_str()) {
            return format!("{left}{right}").into();
        }
        let mut units = self.units();
        units.extend(suffix.units());
        Self::from_utf16(units)
    }
}
impl From<String> for JsString {
    fn from(value: String) -> Self {
        Self(Repr::Utf8(value))
    }
}
impl From<&str> for JsString {
    fn from(value: &str) -> Self {
        value.to_owned().into()
    }
}
impl PartialEq for JsString {
    fn eq(&self, other: &Self) -> bool {
        match (self.as_str(), other.as_str()) {
            (Some(left), Some(right)) => left == right,
            _ => self.units() == other.units(),
        }
    }
}
impl Eq for JsString {}
impl std::hash::Hash for JsString {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.units(), state);
    }
}
impl PartialEq<str> for JsString {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == Some(other)
    }
}
impl PartialEq<&str> for JsString {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}
impl JsString {
    pub(crate) fn array_index(&self) -> Option<u32> {
        let key = self.as_str()?;
        key.parse::<u32>().ok().filter(|i| *i != u32::MAX && i.to_string() == key)
    }
    pub(crate) fn reserved(&self) -> bool {
        self.as_str().is_some_and(|key| crate::delta::RESERVED_SEGMENTS.contains(&key))
    }
}
impl PartialOrd for JsString {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for JsString {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.units().cmp(&other.units())
    }
}
impl Serialize for JsString {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let Some(value) = self.as_str() {
            return serializer.serialize_str(value);
        }
        // Raw JSON is required: Serde's str contract cannot carry a surrogate.
        let mut json = String::from("\"");
        for unit in self.units() {
            use std::fmt::Write;
            write!(json, "\\u{unit:04x}").map_err(serde::ser::Error::custom)?;
        }
        json.push('"');
        serde_json::value::RawValue::from_string(json)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for JsString {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = crate::types::JsonValue::deserialize(deserializer)?;
        match value {
            crate::types::JsonValue::String(value) => Ok(value),
            _ => Err(serde::de::Error::custom("expected string")),
        }
    }
}
