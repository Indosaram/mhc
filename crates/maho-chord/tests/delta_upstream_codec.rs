use maho_chord::{delta::*, JsonValue};
use serde_json::{json, Value};

fn ops(value: Value) -> Result<Vec<Op>, serde_json::Error> {
    serde_json::from_value(value)
}

#[test]
fn decoded_and_wire_vocabularies_are_distinct() -> Result<(), Box<dyn std::error::Error>> {
    for operation in [
        json!(["r",{"value":1}]),
        json!(["s", ["value"], 1]),
        json!(["d", ["value"]]),
        json!(["a", ["value"], "x"]),
        json!(["t", ["value"], 2]),
        json!(["p", ["value"], 0, 0, []]),
        json!(["m", ["value"], [0]]),
    ] {
        assert_valid_op(&operation)?;
    }
    for wire in [
        json!(["s", 1]),
        json!(["d"]),
        json!(["a", "x"]),
        json!(["t", 2]),
        json!(["p", 0, 0, []]),
        json!(["#", 0, ["value"]]),
        json!(["s", 0, 1]),
    ] {
        assert!(assert_valid_op(&wire).is_err());
        assert_valid_wire_op(&wire)?;
    }
    Ok(())
}

#[test]
fn null_containing_paths_do_not_collide_and_mixed_streams_round_trip(
) -> Result<(), Box<dyn std::error::Error>> {
    let operations = ops(json!([["s", ["a\u{0000}b"], 1], ["s", ["a", "b"], 2]]))?;
    assert_eq!(
        decoder().decode(&encoder().encode(&operations))?,
        operations
    );
    let mut encoder = encoder();
    let mut decoder = decoder();
    for index in 0..100 {
        let operations = ops(json!([
            ["s", ["rows", index, "value"], index],
            ["a", ["output"], index.to_string()],
            ["p", ["tail"], index, 0, [index]]
        ]))?;
        assert_eq!(decoder.decode(&encoder.encode(&operations))?, operations);
    }
    Ok(())
}

#[test]
fn replacement_clears_both_path_dictionaries() -> Result<(), Box<dyn std::error::Error>> {
    let mut encoder = encoder();
    let mut decoder = decoder();
    for value in ["1", "2"] {
        let operations = ops(json!([["a", ["a", "deep"], value]]))?;
        assert_eq!(decoder.decode(&encoder.encode(&operations))?, operations);
    }
    let base = ops(json!([["r",{"a":{"deep":"x"}}]]))?;
    decoder.decode(&encoder.encode(&base))?;
    assert!(decoder.decode(&[json!(["a", 0, "bad"])]).is_err());
    let operations = ops(json!([["a", ["a", "deep"], "3"]]))?;
    let wire = encoder.encode(&operations);
    assert_eq!(serde_json::to_value(&wire)?, json!([["a", ["a", "deep"], "3"]]));
    assert_eq!(decoder.decode(&wire)?, operations);
    assert!(decoder.decode(&[json!(["a", "x"])]).is_err());
    Ok(())
}

#[test]
fn array_index_boundaries_and_large_splice_payloads() -> Result<(), Box<dyn std::error::Error>> {
    let base: JsonValue = json!({"values":[1,2,3]}).into();
    for (operation, expected) in [
        (json!([["s", ["values", 1], 9]]), json!({"values":[1,9,3]})),
        (
            json!([["s", ["values", 3], 9]]),
            json!({"values":[1,2,3,9]}),
        ),
        (
            json!([["p", ["values"], 0, 1_000_000_000, []]]),
            json!({"values":[]}),
        ),
    ] {
        assert_eq!(
            apply_immutable(Some(&base), &ops(operation)?)?,
            expected.into()
        );
    }
    for operation in [
        json!([["s", ["values", 5], 9]]),
        json!([["s", ["values", 4_294_967_290u64], 9]]),
        json!([["s", ["values", "0"], 9]]),
        json!([["a", ["values", "0"], "x"]]),
        json!([["d", ["values", 3]]]),
    ] {
        assert!(apply_immutable(Some(&base), &ops(operation)?).is_err());
    }
    let items = vec![JsonValue::Null; 300_000];
    let result = apply_immutable(
        Some(&JsonValue::array(vec![])),
        &[Op::Splice(vec![], 0, 0, items)],
    )?;
    assert_eq!(result.as_array().map(Vec::len), Some(300_000));
    Ok(())
}

#[test]
fn malformed_structures_append_targets_and_truncations_fail() {
    for value in [
        json!(["ZZZ", ["value"], 9]),
        json!(["p", ["values"], 0, 0, "not-an-array"]),
        json!(["s", "value", 9]),
        json!({"op":"s"}),
        Value::Null,
        json!(["t", ["value"], -1]),
    ] {
        assert!(assert_valid_op(&value).is_err());
    }
    for path in [vec!["missing".into()], vec!["value".into()]] {
        assert!(apply_immutable(
            Some(&json!({"value":1}).into()),
            &[Op::Append(path, "x".into())]
        )
        .is_err());
    }
}
