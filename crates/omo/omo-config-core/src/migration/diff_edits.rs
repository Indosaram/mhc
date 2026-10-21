use serde_json::Value;

use crate::internal::plain_object::is_unsafe_object_key;
use crate::issue::PathSegment;
use crate::writer::types::OmoConfigEdit;

fn deep_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, value)| {
                    right.get(key).is_some_and(|other| deep_equal(value, other))
                })
        }
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left.iter().zip(right).all(|(a, b)| deep_equal(a, b))
        }
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        _ => left == right,
    }
}

pub(super) fn without_marker(document: &Value) -> Value {
    let mut document = document.clone();
    if let Some(map) = document.as_object_mut() {
        map.remove("_migrations");
    }
    document
}

pub(super) fn diff_edits(before: &Value, after: &Value, path: &[PathSegment]) -> Vec<OmoConfigEdit> {
    let mut edits = Vec::new();
    let (Some(before), Some(after)) = (before.as_object(), after.as_object()) else {
        return edits;
    };
    for key in before.keys() {
        if !is_unsafe_object_key(key) && !after.contains_key(key) {
            let mut next = path.to_vec();
            next.push(PathSegment::Key(key.clone()));
            edits.push(OmoConfigEdit { path: next, value: None });
        }
    }
    for (key, value) in after {
        if is_unsafe_object_key(key) {
            continue;
        }
        let mut next = path.to_vec();
        next.push(PathSegment::Key(key.clone()));
        if let Some(previous) = before.get(key) {
            if deep_equal(previous, value) {
                continue;
            }
            if previous.is_object() && value.is_object() {
                edits.extend(diff_edits(previous, value, &next));
                continue;
            }
        }
        edits.push(OmoConfigEdit { path: next, value: Some(value.clone()) });
    }
    edits
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nested_diff_removes_keys_and_skips_equal_subtrees() {
        // Given
        let before = json!({"task":{"remove":1,"keep":{"array":[1,{"x":2}]}},"gone":true});
        let after = json!({"task":{"keep":{"array":[1,{"x":2}]},"added":null}});
        // When
        let edits = diff_edits(&before, &after, &[]);
        // Then
        assert_eq!(edits.len(), 3);
        assert_eq!(edits[0].path, vec![PathSegment::Key("gone".into())]);
        assert_eq!(edits[0].value, None);
        assert_eq!(edits[1].path, vec![PathSegment::Key("task".into()), PathSegment::Key("remove".into())]);
        assert_eq!(edits[1].value, None);
        assert_eq!(edits[2].value, Some(Value::Null));
    }

    #[test]
    fn diff_skips_unsafe_keys_and_replaces_arrays() {
        // Given
        let before = json!({"constructor":1,"__proto__":1,"prototype":1,"a":[1]});
        let after = json!({"constructor":2,"__proto__":2,"a":[2]});
        // When
        let edits = diff_edits(&before, &after, &[]);
        // Then
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].path, vec![PathSegment::Key("a".into())]);
        assert_eq!(edits[0].value, Some(json!([2])));
    }

    #[test]
    fn marker_removal_and_object_order_do_not_produce_edits() {
        // Given
        let before = json!({"_migrations":["old"],"a":{"x":1,"y":2}});
        let after = json!({"a":{"y":2,"x":1},"_migrations":["new"]});
        // When
        let edits = diff_edits(&without_marker(&before), &without_marker(&after), &[]);
        // Then
        assert!(edits.is_empty());
    }
}
