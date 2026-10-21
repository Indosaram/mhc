use maho_chord::{delta::*, JsonValue};
use serde_json::{json, Value};
fn check(before: Value, after: Value, expected: Value) {
    let before: JsonValue = before.into();
    let after: JsonValue = after.into();
    let operations = diff_revisions(&before, &after);
    assert_eq!(
        serde_json::to_value(&operations).expect("serialize"),
        expected
    );
    assert_eq!(
        apply_immutable(Some(&before), &operations).expect("replay"),
        after
    );
}
#[test]
fn sets_and_deletions_follow_after_then_before_order() {
    check(
        json!({"keep":1,"change":1,"remove":true}),
        json!({"keep":1,"change":2,"add":3}),
        json!([["s", ["change"], 2], ["s", ["add"], 3], ["d", ["remove"]]]),
    );
}
#[test]
fn strings_emit_append_truncation_and_overlap() {
    check(
        json!({"text":"hello"}),
        json!({"text":"hello world"}),
        json!([["a", ["text"], " world"]]),
    );
    check(
        json!({"text":"hello world"}),
        json!({"text":"world"}),
        json!([["t", ["text"], 6]]),
    );
    check(
        json!({"text":"abcdefgh"}),
        json!({"text":"defghxyz"}),
        json!([["t", ["text"], 3], ["a", ["text"], "xyz"]]),
    );
}
#[test]
fn canonical_array_removals_and_insertions_stay_narrow() {
    for (after, expected) in [
        (json!([2, 3, 4]), json!([["p", ["values"], 0, 1, []]])),
        (json!([1, 2, 3]), json!([["p", ["values"], 3, 1, []]])),
        (json!([1, 3, 4]), json!([["p", ["values"], 1, 1, []]])),
        (json!([]), json!([["p", ["values"], 0, 4, []]])),
        (json!([1, 2, 3, 4]), json!([])),
    ] {
        check(
            json!({"values":[1,2,3,4]}),
            json!({"values":after}),
            expected,
        );
    }
    check(
        json!({"values":[1,3]}),
        json!({"values":[1,2,3]}),
        json!([["p", ["values"], 1, 0, [2]]]),
    );
}
#[test]
fn shifted_reconstructed_objects_do_not_get_field_diffed() {
    check(
        json!({"values":[{"type":"row","value":1},{"type":"row","value":2},{"type":"row","value":3}]}),
        json!({"values":[{"type":"row","value":2},{"type":"row","value":3},{"type":"row","value":4}]}),
        json!([["p",["values"],0,1,[]],["p",["values"],2,0,[{"type":"row","value":4}]]]),
    );
}
#[test]
fn reconstructed_leaf_edit_is_narrow() {
    check(
        json!({"values":[{"id":1,"label":"one"},{"id":2,"label":"two"}]}),
        json!({"values":[{"id":1,"label":"one"},{"id":2,"label":"changed"}]}),
        json!([["s", ["values", 1, "label"], "changed"]]),
    );
}
#[test]
fn reserved_key_mutations_fold_to_safe_container() {
    check(
        json!({"nested":{"constructor":1,"x":1}}),
        json!({"nested":{"constructor":2,"x":1}}),
        json!([["s",["nested"],{"constructor":2,"x":1}]]),
    );
}
#[test]
fn identity_rotation_emits_payload_free_permutation() {
    let values: Vec<JsonValue> = (0..10_000).map(|i| json!({"value":i}).into()).collect();
    let rotated = values[1000..]
        .iter()
        .chain(values[..1000].iter())
        .cloned()
        .collect();
    let before = JsonValue::array(values);
    let after = JsonValue::array(rotated);
    let operations = diff_revisions(&before, &after);
    assert!(
        matches!(operations.as_slice(),[Op::Permute(path,order)] if path.is_empty() && order.len() == 10_000)
    );
    assert_eq!(
        apply_immutable(Some(&before), &operations).expect("replay"),
        after
    );
}
#[test]
fn equal_reconstructed_values_are_noops() {
    check(
        json!({"nested":[1,{"x":2}]}),
        json!({"nested":[1,{"x":2}]}),
        json!([]),
    );
}
#[test]
fn wide_objects_exceeding_operation_cap_replace_root() {
    let before = JsonValue::object(
        (0..5000)
            .map(|i| (format!("field{i}").into(), json!(0).into()))
            .collect(),
    );
    let after = JsonValue::object(
        (0..5000)
            .map(|i| (format!("field{i}").into(), json!(1).into()))
            .collect(),
    );
    assert_eq!(diff_revisions(&before, &after), vec![Op::Replace(after)]);
}
