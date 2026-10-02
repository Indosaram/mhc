use serde::{Deserialize,Deserializer};

/// With serde(default), missing is None and explicit null is Some(None).
pub fn deserialize<'de,D,T>(deserializer:D)->Result<Option<Option<T>>,D::Error>
where D:Deserializer<'de>,T:Deserialize<'de> {
    Option::<T>::deserialize(deserializer).map(Some)
}
