use serde_json::{Map, Value};

use crate::record_values::copy_record;

fn equal_values(left: Option<&Value>, right: &Value) -> bool {
    let Some(left) = left else {
        return false;
    };
    match (left, right) {
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| equal_values(Some(left), right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, value)| {
                    right
                        .get(key)
                        .is_some_and(|other| equal_values(Some(value), other))
                })
        }
        (left, right) => left == right,
    }
}

/// Keys of `profile` whose values differ from `base`, recursing into nested records.
pub fn deep_difference(
    base: &Map<String, Value>,
    profile: &Map<String, Value>,
) -> Map<String, Value> {
    let mut difference = Map::new();
    for (key, profile_value) in profile {
        let base_value = base.get(key);
        if let (Some(Value::Object(base_record)), Value::Object(profile_record)) =
            (base_value, profile_value)
        {
            let nested = deep_difference(base_record, profile_record);
            if !nested.is_empty() {
                difference.insert(key.clone(), Value::Object(nested));
            }
            continue;
        }
        if !equal_values(base_value, profile_value) {
            let value = match profile_value {
                Value::Object(record) => Value::Object(copy_record(record)),
                other => other.clone(),
            };
            difference.insert(key.clone(), value);
        }
    }
    difference
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::deep_difference;

    fn record(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn deep_difference_keeps_only_changed_leaves() {
        // given
        let base = record(json!({ "a": { "b": 1, "c": [1, 2] }, "d": "same" }));
        let profile = record(json!({ "a": { "b": 2, "c": [1, 2] }, "d": "same", "e": { "f": 1 } }));

        // when
        let difference = deep_difference(&base, &profile);

        // then
        assert_eq!(
            serde_json::Value::Object(difference),
            json!({ "a": { "b": 2 }, "e": { "f": 1 } })
        );
    }
}
