use serde_json::{Map, Value};
pub fn is_record(value: &Value) -> bool { value.is_object() }
pub fn as_record(value: &Value) -> Option<&Map<String, Value>> { value.as_object() }
pub fn as_array(value: &Value) -> &[Value] { value.as_array().map_or(&[], Vec::as_slice) }
pub fn finite_number(value: &Value) -> Option<f64> { value.as_f64().filter(|n| n.is_finite()) }
pub fn non_empty_string(value: &Value) -> Option<&str> { value.as_str().filter(|s| !s.is_empty()) }
pub fn optional<K: Ord, V>(key: K, value: Option<V>) -> std::collections::BTreeMap<K, V> { value.map(|v| (key, v)).into_iter().collect() }
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test] fn narrowing_is_total() {
        for value in [json!([]), Value::Null, json!("x"), json!(1)] { assert!(!is_record(&value)); assert!(as_record(&value).is_none()); }
        let object = json!({"a": 1}); assert!(std::ptr::eq(as_record(&object).expect("object"), object.as_object().expect("object")));
        assert!(as_array(&json!({"length":2})).is_empty()); assert_eq!(as_array(&json!([1])), &[json!(1)]);
        assert!(finite_number(&json!("3")).is_none()); assert_eq!(finite_number(&json!(0)), Some(0.0));
        assert!(non_empty_string(&json!("")).is_none()); assert_eq!(non_empty_string(&json!(" ")), Some(" ")); assert!(non_empty_string(&json!(7)).is_none());
        assert!(optional::<_, i32>("a", None).is_empty()); assert_eq!(optional("a", Some(0))["a"], 0); assert_eq!(optional("a", Some(""))["a"], "");
    }
}
