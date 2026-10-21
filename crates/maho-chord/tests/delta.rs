use maho_chord::{delta::*, JsonValue};
use serde_json::{json, Value};

fn value(v: Value) -> JsonValue {
    v.into()
}
fn ops(v: Value) -> Vec<Op> {
    serde_json::from_value(v).expect("valid fixture")
}

#[test]
fn codec_interns_second_use_and_resets_at_replacement() {
    let mut encoder = encoder();
    let mut decoder = decoder();
    let first = ops(json!([["s", ["text"], "a"], ["a", ["text"], "b"]]));
    let second = ops(json!([["t", ["text"], 1]]));
    let wire = encoder.encode(&first);
    assert_eq!(serde_json::to_value(&wire).expect("serialize"), json!([["s", ["text"], "a"], ["a", "b"]]));
    assert_eq!(decoder.decode(&wire).expect("decode"), first);
    let wire = encoder.encode(&second);
    assert_eq!(serde_json::to_value(&wire).expect("serialize"), json!([["#", 0, ["text"]], ["t", 0, 1]]));
    assert_eq!(decoder.decode(&wire).expect("decode"), second);
    let recovery = ops(json!([["r",{"text":"x"}],["a",["text"],"y"]]));
    let wire = encoder.encode(&recovery);
    assert_eq!(
        maho_chord::delta::decoder()
            .decode(&wire)
            .expect("fresh recovery"),
        recovery
    );
}

#[test]
fn permutations_support_root_and_short_forms() {
    let operations = ops(json!([["m", [], [2, 0, 1]], ["m", [], [1, 2, 0]]]));
    let wire = encoder().encode(&operations);
    assert_eq!(
        serde_json::to_value(&wire).expect("serialize"),
        json!([["m", [], [2, 0, 1]], ["m", [1, 2, 0]]])
    );
    assert_eq!(decoder().decode(&wire).expect("decode"), operations);
    assert_eq!(
        apply(Some(value(json!([1, 2, 3]))), &operations).expect("apply"),
        value(json!([1, 2, 3]))
    );
}

#[test]
fn malformed_operations_are_rejected_before_replay() {
    for fixture in [
        json!(["s", [], 1]),
        json!(["s", ["__proto__", "x"], true]),
        json!(["a", ["x"], 1]),
        json!(["t", ["x"], -1]),
        json!(["m", [], [0, 0]]),
        json!(["m", [], [1]]),
        json!(["p", [], 0, 1, {}]),
        json!(["unknown", []]),
    ] {
        assert!(assert_valid_op(&fixture).is_err());
    }
    assert!(decoder().decode(&[json!(["d"])]).is_err());
    assert!(decoder().decode(&[json!(["s", 7, 1])]).is_err());
    assert!(decoder()
        .decode(&[json!(["#", 0, []]), json!(["s", 0, 1])])
        .is_err());
}

#[test]
fn sparse_and_string_keyed_array_writes_are_rejected() {
    let base = value(json!([1, 2]));
    for operation in [
        Op::Set(vec![Seg::Index(3)], value(json!(3))),
        Op::Set(vec![Seg::Key("0".into())], value(json!(3))),
    ] {
        assert!(apply_immutable(Some(&base), &[operation]).is_err());
    }
    assert_eq!(base, value(json!([1, 2])));
}

#[test]
fn immutable_replay_preserves_payload_and_untouched_container_identity() {
    let base = value(json!({"text":"abcdef","stable":{"n":7},"placed":null,"rows":[1,2,3]}));
    let payload = value(json!({"nested":{"n":1}}));
    let operations = vec![
        Op::Set(vec!["placed".into()], payload.clone()),
        Op::Set(
            vec!["placed".into(), "nested".into(), "n".into()],
            value(json!(2)),
        ),
        Op::Truncate(vec!["text".into()], 2),
        Op::Append(vec!["text".into()], "!".into()),
        Op::Splice(
            vec!["rows".into()],
            1,
            1,
            vec![value(json!(4)), value(json!(5))],
        ),
        Op::Permute(vec!["rows".into()], vec![3, 0, 1, 2]),
    ];
    let result = apply_immutable(Some(&base), &operations).expect("apply");
    assert_eq!(
        result,
        value(
            json!({"text":"cdef!","stable":{"n":7},"placed":{"nested":{"n":2}},"rows":[3,1,4,5]})
        )
    );
    assert!(resolve_value(&result, &["stable".into()])
        .expect("stable")
        .same_identity(resolve_value(&base, &["stable".into()]).expect("stable")));
    assert_eq!(payload, value(json!({"nested":{"n":1}})));
    assert_eq!(
        resolve_value(&base, &["text".into()]).expect("text"),
        &value(json!("abcdef"))
    );
}

#[test]
fn batch_partitions_protect_replacement_payloads() {
    let replacement = value(json!({"n":{"x":1},"values":[1,2,3]}));
    let batches = [
        vec![Op::Replace(replacement.clone())],
        ops(json!([["s", ["n", "x"], 2]])),
        ops(json!([
            ["p", ["values"], 1, 1, [4, 5]],
            ["m", ["values"], [3, 0, 1, 2]]
        ])),
    ];
    let result = apply_immutable_batches(None, batches.iter().map(Vec::as_slice)).expect("replay");
    assert_eq!(result, value(json!({"n":{"x":2},"values":[3,1,4,5]})));
    assert_eq!(replacement, value(json!({"n":{"x":1},"values":[1,2,3]})));
}

#[test]
fn overlap_uses_bounded_utf16_units() {
    assert_eq!(overlap("abcdefgh", "defghxyz", 65_536, 64, 8), 5);
    assert_eq!(overlap("abcdef", "defghi", 0, 64, 8), 0);
    assert_eq!(overlap("a😀b", "😀bc", 65_536, 64, 8), 3);
}
