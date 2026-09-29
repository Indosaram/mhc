//! A minimal model of the JavaScript objects the TypeScript package mutates.
//!
//! Byte-compatibility with `JSON.stringify(state, null, 2)` needs two JavaScript object
//! properties that `serde_json::Map` cannot express:
//!
//! * a key assigned `undefined` keeps its slot (and position) but is omitted when
//!   serialized, so re-assigning it later does not move it to the end;
//! * array-index keys (`"0"`, `"7"`, ...) enumerate before all other keys, ascending.
//!
//! Only objects need this treatment; arrays and scalars stay `serde_json::Value`.

use serde_json::{Map, Value};

/// A JavaScript property value: `undefined`, a mutable object, or any other JSON value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Js {
    Undefined,
    Object(JsObj),
    /// Never `Value::Object`; objects are always converted to [`Js::Object`].
    Value(Value),
}

/// An insertion-ordered JavaScript object.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JsObj {
    entries: Vec<(String, Js)>,
}

impl Js {
    /// Convert parsed JSON; integral floats become integers because JavaScript has a
    /// single number type and `JSON.stringify(1.0)` is `1`.
    pub(crate) fn from_value(value: Value) -> Self {
        const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
        match value {
            Value::Object(map) => Self::Object(JsObj::from_map(map)),
            Value::Number(number) => match number.as_f64() {
                Some(float)
                    if number.is_f64()
                        && float.fract() == 0.0
                        && float.abs() <= MAX_SAFE_INTEGER =>
                {
                    Self::Value(Value::from(float as i64))
                }
                Some(_) | None => Self::Value(Value::Number(number)),
            },
            other => Self::Value(other),
        }
    }

    pub(crate) fn string(text: &str) -> Self {
        Self::Value(Value::String(text.to_string()))
    }

    pub(crate) fn int(number: i64) -> Self {
        Self::Value(Value::from(number))
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Self::Value(Value::String(text)) => Some(text),
            Self::Undefined | Self::Object(_) | Self::Value(_) => None,
        }
    }

    pub(crate) fn as_object(&self) -> Option<&JsObj> {
        match self {
            Self::Object(object) => Some(object),
            Self::Undefined | Self::Value(_) => None,
        }
    }

    pub(crate) fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Self::Value(Value::Array(items)) => Some(items),
            Self::Undefined | Self::Object(_) | Self::Value(_) => None,
        }
    }

    pub(crate) fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Value(value) => value.as_i64(),
            Self::Undefined | Self::Object(_) => None,
        }
    }

    /// `undefined` or `null` (the operands `??` skips).
    pub(crate) fn is_nullish(&self) -> bool {
        matches!(self, Self::Undefined | Self::Value(Value::Null))
    }

    /// JavaScript truthiness.
    pub(crate) fn truthy(&self) -> bool {
        match self {
            Self::Undefined => false,
            Self::Object(_) => true,
            Self::Value(value) => match value {
                Value::Null => false,
                Value::Bool(flag) => *flag,
                Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
                Value::String(text) => !text.is_empty(),
                Value::Array(_) | Value::Object(_) => true,
            },
        }
    }

    /// `String(value)`, as used by template literals and property keys.
    pub(crate) fn to_js_string(&self) -> String {
        match self {
            Self::Undefined => "undefined".to_string(),
            Self::Object(_) => "[object Object]".to_string(),
            Self::Value(value) => match value {
                Value::String(text) => text.clone(),
                Value::Null => "null".to_string(),
                Value::Bool(flag) => flag.to_string(),
                Value::Number(number) => number.to_string(),
                Value::Array(items) => items
                    .iter()
                    .map(|item| match item {
                        Value::Null => String::new(),
                        other => Self::from_value(other.clone()).to_js_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(","),
                Value::Object(_) => "[object Object]".to_string(),
            },
        }
    }

    /// Strict equality (`===`) for the scalar values ids are compared by.
    pub(crate) fn strict_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Undefined, Self::Undefined) => true,
            (Self::Value(left), Self::Value(right)) => match (left, right) {
                (Value::Array(_), _) | (_, Value::Array(_)) => false,
                (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
                _ => left == right,
            },
            _ => false,
        }
    }

    /// `{ ...value }` for the values this package spreads: objects are copied, anything
    /// else spreads to an empty object.
    pub(crate) fn spread(&self) -> JsObj {
        self.as_object().cloned().unwrap_or_default()
    }

    /// Serializable form; `None` for `undefined`.
    pub(crate) fn to_value(&self) -> Option<Value> {
        match self {
            Self::Undefined => None,
            Self::Object(object) => Some(Value::Object(object.to_map())),
            Self::Value(value) => Some(value.clone()),
        }
    }
}

impl JsObj {
    pub(crate) fn from_map(map: Map<String, Value>) -> Self {
        let mut object = Self::default();
        for (key, value) in map {
            object.set(&key, Js::from_value(value));
        }
        object
    }

    /// `Object.fromEntries(entries)`: a later duplicate key overwrites in place.
    pub(crate) fn from_entries(entries: impl IntoIterator<Item = (String, Js)>) -> Self {
        let mut object = Self::default();
        for (key, value) in entries {
            object.set(&key, value);
        }
        object
    }

    /// Property read: `None` when the key is missing or holds `undefined`.
    pub(crate) fn get(&self, key: &str) -> Option<&Js> {
        self.entries
            .iter()
            .find(|(existing, _)| existing == key)
            .map(|(_, value)| value)
            .filter(|value| !matches!(value, Js::Undefined))
    }

    /// Raw property read, `undefined` included, for copying a field verbatim.
    pub(crate) fn field(&self, key: &str) -> Js {
        self.get(key).cloned().unwrap_or(Js::Undefined)
    }

    /// `object[key] ?? fallback` helper: `None` when missing, `undefined` or `null`.
    pub(crate) fn coalesce(&self, key: &str) -> Option<&Js> {
        self.get(key).filter(|value| !value.is_nullish())
    }

    pub(crate) fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Js::as_str)
    }

    pub(crate) fn get_obj(&self, key: &str) -> Option<&JsObj> {
        self.get(key).and_then(Js::as_object)
    }

    /// Mutable views of every object-valued property.
    pub(crate) fn objects_mut(&mut self) -> impl Iterator<Item = &mut JsObj> {
        self.entries
            .iter_mut()
            .filter_map(|(_, value)| match value {
                Js::Object(object) => Some(object),
                Js::Undefined | Js::Value(_) => None,
            })
    }

    pub(crate) fn get_obj_mut(&mut self, key: &str) -> Option<&mut JsObj> {
        self.entries
            .iter_mut()
            .find(|(existing, _)| existing == key)
            .and_then(|(_, value)| match value {
                Js::Object(object) => Some(object),
                Js::Undefined | Js::Value(_) => None,
            })
    }

    /// Property assignment: an existing key keeps its position, a new key is appended.
    pub(crate) fn set(&mut self, key: &str, value: Js) {
        match self
            .entries
            .iter_mut()
            .find(|(existing, _)| existing == key)
        {
            Some((_, slot)) => *slot = value,
            None => self.entries.push((key.to_string(), value)),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries in JavaScript enumeration order (array indices first, ascending).
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, &Js)> {
        let mut ordered: Vec<(&str, &Js)> = self
            .entries
            .iter()
            .map(|(key, value)| (key.as_str(), value))
            .collect();
        ordered.sort_by_key(|(key, _)| array_index(key).map_or((1, 0), |index| (0, index)));
        ordered.into_iter()
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &Js> {
        self.iter().map(|(_, value)| value)
    }

    pub(crate) fn to_map(&self) -> Map<String, Value> {
        self.iter()
            .filter_map(|(key, value)| value.to_value().map(|value| (key.to_string(), value)))
            .collect()
    }

    /// `JSON.stringify(object, null, 2)`.
    pub(crate) fn stringify_pretty(&self) -> String {
        format!("{:#}", Value::Object(self.to_map()))
    }
}

/// Canonical array-index key (`"0"`, `"1"`, ... below 2^32 - 1), as JavaScript orders them.
fn array_index(key: &str) -> Option<u64> {
    let canonical = key == "0" || (!key.starts_with('0') && !key.is_empty());
    if !canonical || !key.bytes().all(|byte| byte.is_ascii_digit()) || key.len() > 10 {
        return None;
    }
    key.parse::<u64>()
        .ok()
        .filter(|index| *index < u64::from(u32::MAX))
}
