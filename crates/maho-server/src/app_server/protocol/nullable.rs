use serde::{Deserialize,Deserializer};

/// With serde(default), missing is None and explicit null is Some(None).
pub fn deserialize<'de,D,T>(deserializer:D)->Result<Option<Option<T>>,D::Error>
where D:Deserializer<'de>,T:Deserialize<'de> {
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Preserve explicit null when T itself represents JSON null.
pub fn deserialize_present<'de,D,T>(deserializer:D)->Result<Option<T>,D::Error>
where D:Deserializer<'de>,T:Deserialize<'de> {
    T::deserialize(deserializer).map(Some)
}

/// Nullable is not optional: the key must still be supplied.
pub fn deserialize_required<'de,D,T>(deserializer:D)->Result<Option<T>,D::Error>
where D:Deserializer<'de>,T:Deserialize<'de> {
    Option::<T>::deserialize(deserializer)
}
