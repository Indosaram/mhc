use maho_chord::{delta::*, JsonValue};
use std::result::Result;
use serde_json::json;

fn check(before: JsonValue, after: JsonValue, expected: Vec<Op>) -> Result<(), DeltaError> {
    let operations = diff_revisions(&before, &after);
    assert_eq!(operations, expected);
    assert_eq!(apply_immutable(Some(&before), &operations)?, after);
    Ok(())
}
fn rows(values: Vec<JsonValue>) -> JsonValue {
    JsonValue::object(vec![("values".into(), JsonValue::array(values))])
}

#[test]
fn shared_rows_insert_remove_shift_and_permute_canonically() -> Result<(), DeltaError> {
    let a: JsonValue = json!({"id":"a"}).into();
    let b: JsonValue = json!({"id":"b"}).into();
    let c: JsonValue = json!({"id":"c"}).into();
    let d: JsonValue = json!({"id":"d"}).into();
    let p = vec!["values".into()];
    check(
        rows(vec![a.clone(), b.clone()]),
        rows(vec![a.clone(), c.clone(), b.clone()]),
        vec![Op::Splice(p.clone(), 1, 0, vec![c.clone()])],
    )?;
    check(
        rows(vec![a.clone(), b.clone(), c.clone()]),
        rows(vec![b.clone(), c.clone()]),
        vec![Op::Splice(p.clone(), 0, 1, vec![])],
    )?;
    check(
        rows(vec![a.clone(), b.clone(), c.clone()]),
        rows(vec![b.clone(), c.clone(), d.clone()]),
        vec![
            Op::Splice(p.clone(), 0, 1, vec![]),
            Op::Splice(p.clone(), 2, 0, vec![d]),
        ],
    )?;
    check(
        rows(vec![a.clone(), b.clone(), c.clone()]),
        rows(vec![c, a, b]),
        vec![Op::Permute(p, vec![2, 0, 1])],
    )
}

#[test]
fn removed_large_neighbors_never_enter_operation_payloads() -> Result<(), Box<dyn std::error::Error>>
{
    for size in [256 * 1024, 1024 * 1024] {
        let values: Vec<JsonValue> = (0..5)
            .map(|i| json!({"id":i,"payload":"x".repeat(size)}).into())
            .collect();
        let before = rows(values.clone());
        let after = rows(vec![
            values[0].clone(),
            values[2].clone(),
            values[4].clone(),
        ]);
        let operations = diff_revisions(&before, &after);
        assert_eq!(
            operations,
            vec![
                Op::Splice(vec!["values".into()], 1, 1, vec![]),
                Op::Splice(vec!["values".into()], 2, 1, vec![])
            ]
        );
        assert!(serde_json::to_string(&operations)?.len() < 100);
        assert_eq!(apply_immutable(Some(&before), &operations)?, after);
    }
    Ok(())
}

#[test]
fn push_pop_and_middle_removal_stay_narrow_for_large_arrays() -> Result<(), DeltaError> {
    for size in [1001, 10_000] {
        let values: Vec<JsonValue> = (0..size).map(|i| json!({"value":i}).into()).collect();
        let before = rows(values.clone());
        let appended: JsonValue = json!({"value":size}).into();
        let mut after = values.clone();
        after.push(appended.clone());
        check(
            before.clone(),
            rows(after),
            vec![Op::Splice(vec!["values".into()], size, 0, vec![appended])],
        )?;
        check(
            before.clone(),
            rows(values[..size - 1].to_vec()),
            vec![Op::Splice(vec!["values".into()], size - 1, 1, vec![])],
        )?;
        let mut after = values;
        after.remove(size / 2);
        check(
            before,
            rows(after),
            vec![Op::Splice(vec!["values".into()], size / 2, 1, vec![])],
        )?;
    }
    Ok(())
}

#[test]
fn forty_thousand_sparse_shared_subtree_edits_stay_narrow() -> Result<(), Box<dyn std::error::Error>>
{
    let values: Vec<JsonValue> = (0..40_000)
        .map(|i| json!({"value":i,"stable":{"value":i}}).into())
        .collect();
    let mut after = values.clone();
    let mut expected = Vec::new();
    for index in (100..40_000).step_by(400) {
        let stable = resolve_value(&values[index], &["stable".into()])?.clone();
        let number = -i64::try_from(index)?;
        after[index] = JsonValue::object(vec![
            ("value".into(), json!(number).into()),
            ("stable".into(), stable),
        ]);
        expected.push(Op::Set(
            vec!["values".into(), index.into(), "value".into()],
            json!(number).into(),
        ));
    }
    let before = rows(values);
    let after = rows(after);
    let operations = diff_revisions(&before, &after);
    assert_eq!(operations, expected);
    assert!(serde_json::to_string(&operations)?.len() < 7500);
    assert_eq!(apply_immutable(Some(&before), &operations)?, after);
    Ok(())
}

#[test]
fn reconstructed_large_leaf_edit_and_ambiguous_moved_gap() -> Result<(), Box<dyn std::error::Error>>
{
    let values: Vec<JsonValue> = (0..1000)
        .map(|i| json!({"value":i,"label":format!("row-{i}")}).into())
        .collect();
    let before = rows(values);
    let mut after = maho_chord::json::copy_json(&before, Default::default());
    after = apply_immutable(
        Some(&after),
        &[Op::Set(
            vec!["values".into(), 700usize.into(), "label".into()],
            json!("changed").into(),
        )],
    )?;
    check(
        before,
        after,
        vec![Op::Set(
            vec!["values".into(), 700usize.into(), "label".into()],
            json!("changed").into(),
        )],
    )?;
    let before: JsonValue = json!({"values":[{"value":"left"},{"id":1,"key":"a","value":"first"},{"id":2,"key":"b","value":"second"},{"value":"right"}]}).into();
    let replacements: Vec<JsonValue> = vec![
        json!({"id":2,"key":"b","value":"edited-second"}).into(),
        json!({"id":1,"key":"a","value":"edited-first"}).into(),
    ];
    let after = apply_immutable(
        Some(&before),
        &[Op::Splice(
            vec!["values".into()],
            1,
            2,
            replacements.clone(),
        )],
    )?;
    check(
        before,
        after,
        vec![Op::Splice(vec!["values".into()], 1, 2, replacements)],
    )?;
    Ok(())
}

#[test]
fn five_hundred_unshifts_do_not_snapshot_retained_rows() -> Result<(), Box<dyn std::error::Error>> {
    let retained: Vec<JsonValue> = (0..10_000)
        .map(|i| json!({"value":i,"payload":"x".repeat(100)}).into())
        .collect();
    let inserted: Vec<JsonValue> = (0..500).map(|i| json!({"value":-i-1}).into()).collect();
    let before = rows(retained.clone());
    let after = rows(inserted.iter().chain(retained.iter()).cloned().collect());
    let operations = diff_revisions(&before, &after);
    assert_eq!(
        operations,
        vec![Op::Splice(vec!["values".into()], 0, 0, inserted)]
    );
    assert!(serde_json::to_string(&operations)?.len() < 20_000);
    assert_eq!(apply_immutable(Some(&before), &operations)?, after);
    Ok(())
}

#[test]
fn wide_array_payload_cost_falls_back_to_root_replacement() {
    let before = rows(vec![json!(0).into(); 40_000]);
    let after = rows(vec![json!(1).into(); 40_000]);
    assert_eq!(diff_revisions(&before, &after), vec![Op::Replace(after)]);
}

#[test]
fn shared_subtree_survivor_edit_combines_with_removal_and_append() -> Result<(), DeltaError> {
    let values: Vec<JsonValue> = ["a", "b", "c", "d"]
        .into_iter()
        .map(|id| json!({"id":id,"stable":{},"detail":{"text":id}}).into())
        .collect();
    let changed = JsonValue::object(vec![
        ("id".into(), json!("c").into()),
        (
            "stable".into(),
            resolve_value(&values[2], &["stable".into()])?.clone(),
        ),
        ("detail".into(), json!({"text":"changed"}).into()),
    ]);
    let appended: JsonValue = json!({"id":"e","stable":{},"detail":{"text":"e"}}).into();
    check(
        rows(values.clone()),
        rows(vec![
            values[0].clone(),
            changed,
            values[3].clone(),
            appended.clone(),
        ]),
        vec![
            Op::Splice(vec!["values".into()], 1, 1, vec![]),
            Op::Append(
                vec![
                    "values".into(),
                    1usize.into(),
                    "detail".into(),
                    "text".into(),
                ],
                "hanged".into(),
            ),
            Op::Splice(vec!["values".into()], 3, 0, vec![appended]),
        ],
    )
}

#[test]
fn equal_length_large_semantic_region_does_not_field_diff_positionally() -> Result<(), DeltaError> {
    let stable: JsonValue = json!({"shared":true}).into();
    let before: Vec<JsonValue> = (0..300)
        .map(|i| {
            JsonValue::object(vec![
                ("value".into(), json!(i).into()),
                ("stable".into(), stable.clone()),
            ])
        })
        .collect();
    let after: Vec<JsonValue> = (0..300)
        .map(|i| {
            JsonValue::object(vec![
                ("value".into(), json!(i + 1000).into()),
                ("stable".into(), stable.clone()),
            ])
        })
        .collect();
    check(
        rows(before),
        rows(after.clone()),
        vec![Op::Splice(vec!["values".into()], 0, 300, after)],
    )
}

#[test]
fn all_canonical_removal_positions_and_deep_equal_reconstruction(
) -> Result<(), Box<dyn std::error::Error>> {
    for (after, expected) in [
        (json!([2, 3, 4]), json!([["p", ["values"], 0, 1, []]])),
        (json!([1, 2, 3]), json!([["p", ["values"], 3, 1, []]])),
        (json!([1, 3, 4]), json!([["p", ["values"], 1, 1, []]])),
        (json!([]), json!([["p", ["values"], 0, 4, []]])),
        (json!([1, 2, 3, 4]), json!([])),
    ] {
        let expected: Vec<Op> = serde_json::from_value(expected)?;
        check(
            json!({"values":[1,2,3,4]}).into(),
            JsonValue::object(vec![("values".into(), after.into())]),
            expected,
        )?;
    }
    for value in [
        json!({"value":{"nested":[1,2]}}),
        json!({"values":[{"id":1},{"id":2}]}),
        json!({"values":[true,true,true]}),
    ] {
        assert!(diff_revisions(&value.clone().into(), &value.into()).is_empty());
    }
    let first: JsonValue = json!({"nested":{"value":1}}).into();
    let second: JsonValue = json!({"nested":{"value":1}}).into();
    assert!(!first.same_identity(&second));
    assert!(diff_revisions(
        &rows(vec![first.clone(), second.clone()]),
        &rows(vec![second, first])
    )
    .is_empty());
    check(json!({"values":[{"type":"row","value":1},{"type":"row","value":2},{"type":"row","value":3}]}).into(),json!({"values":[{"type":"row","value":2},{"type":"row","value":3},{"type":"row","value":4}]}).into(),vec![Op::Splice(vec!["values".into()],0,1,vec![]),Op::Splice(vec!["values".into()],2,0,vec![json!({"type":"row","value":4}).into()])])?;
    Ok(())
}
