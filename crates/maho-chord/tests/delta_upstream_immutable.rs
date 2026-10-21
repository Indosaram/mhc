use maho_chord::{delta::*, JsonValue};
use std::result::Result;
use serde_json::json;

#[test]
fn touched_containers_and_placements_keep_payload_ownership(
) -> Result<(), Box<dyn std::error::Error>> {
    let shared: JsonValue = json!({"nested":{"value":1}}).into();
    let untouched: JsonValue = json!({"value":9}).into();
    let row: JsonValue = json!({"id":4,"label":"placed"}).into();
    let base: JsonValue = json!({"text":"abcdef","stable":{"value":7},"branch":{"value":1},"copy":null,"placed":null,"untouched":null,"left":null,"right":null,"meta":{"count":0,"obsolete":true},"rows":[{"id":1,"label":"one"},{"id":2,"label":"two"},{"id":3,"label":"three"}]}).into();
    let operations = vec![
        Op::Truncate(vec!["text".into()], 2),
        Op::Append(vec!["text".into()], "!".into()),
        Op::Set(vec!["meta".into(), "count".into()], json!(1).into()),
        Op::Set(vec!["meta".into(), "count".into()], json!(2).into()),
        Op::Delete(vec!["meta".into(), "obsolete".into()]),
        Op::Set(vec!["copy".into()], base["branch"].clone()),
        Op::Set(vec!["copy".into(), "value".into()], json!(2).into()),
        Op::Set(vec!["placed".into()], shared.clone()),
        Op::Set(
            vec!["placed".into(), "nested".into(), "value".into()],
            json!(2).into(),
        ),
        Op::Set(vec!["untouched".into()], untouched.clone()),
        Op::Set(vec!["left".into()], shared.clone()),
        Op::Set(vec!["right".into()], shared.clone()),
        Op::Set(
            vec!["left".into(), "nested".into(), "value".into()],
            json!(3).into(),
        ),
        Op::Splice(vec!["rows".into()], 1, 1, vec![row.clone()]),
        Op::Set(
            vec!["rows".into(), 1usize.into(), "label".into()],
            json!("edited").into(),
        ),
        Op::Permute(vec!["rows".into()], vec![1, 0, 2]),
        Op::Set(
            vec!["rows".into(), 0usize.into(), "label".into()],
            json!("moved").into(),
        ),
    ];
    let result = apply_immutable(Some(&base), &operations)?;
    assert_eq!(result, apply(Some(base.clone()), &operations)?);
    assert_eq!(result["text"], json!("cdef!").into());
    assert!(result["stable"].same_identity(&base["stable"]));
    assert!(result["untouched"].same_identity(&untouched));
    assert!(!result["copy"].same_identity(&base["branch"]));
    assert!(!result["left"].same_identity(&shared));
    assert!(result["right"].same_identity(&shared));
    assert_eq!(result["placed"], json!({"nested":{"value":2}}).into());
    assert_eq!(result["rows"][0], json!({"id":4,"label":"moved"}).into());
    assert_eq!(shared, json!({"nested":{"value":1}}).into());
    assert_eq!(row, json!({"id":4,"label":"placed"}).into());
    assert_eq!(base["branch"], json!({"value":1}).into());
    Ok(())
}

#[test]
fn arbitrary_batch_partitions_equal_flattened_and_sequential_replay(
) -> Result<(), Box<dyn std::error::Error>> {
    let initial: JsonValue = json!({"text":"start","values":(0..8).map(|id|json!({"id":id,"score":0})).collect::<Vec<_>>(),"revision":0}).into();
    let mut tracker = track(initial.clone())?;
    let mut batches = Vec::new();
    for revision in 1..=40 {
        let mut change = tracker.begin_change();
        let root = change.state()?;
        root.set("revision", &json!(revision).into())?;
        let old = root.value()?;
        let JsonValue::String(text) = &old["text"] else {
            return Err("expected text".into());
        };
        root.set(
            "text",
            &JsonValue::String(text.slice(1).concat(&revision.to_string().into())),
        )?;
        let values = root.at(&["values".into()])?;
        match revision % 5 {
            0 => values.reverse()?,
            1 => {
                values.push(&[json!({"id":100+revision,"score":revision}).into()])?;
            }
            2 => {
                values.shift()?;
            }
            3 => values
                .get(revision % values.len()?)?
                .ok_or(DeltaError::Root)?
                .set("score", &json!(revision).into())?,
            _ => {
                values.splice(1, 1, &[json!({"id":200+revision,"score":revision}).into()])?;
            }
        }
        let prepared = change.prepare()?;
        batches.push(prepared.ops().to_vec());
        tracker.adopt(&prepared)?;
    }
    let streamed = apply_immutable_batches(Some(&initial), batches.iter().map(Vec::as_slice))?;
    let flattened: Vec<Op> = batches.iter().flatten().cloned().collect();
    assert_eq!(streamed, apply_immutable(Some(&initial), &flattened)?);
    assert_eq!(&streamed, tracker.value());
    let intermediate = apply_immutable(Some(&initial), &batches[0])?;
    let snapshot = intermediate.clone();
    let mut sequential = intermediate.clone();
    for batch in &batches[1..] {
        sequential = apply_immutable(Some(&sequential), batch)?;
    }
    assert_eq!(streamed, sequential);
    assert_eq!(intermediate, snapshot);
    Ok(())
}

#[test]
fn invalid_batches_stop_iteration_and_never_publish_partial_edits() -> Result<(), DeltaError> {
    let base: JsonValue = json!({"nested":{"value":1}}).into();
    let first = [Op::Set(
        vec!["nested".into(), "value".into()],
        json!(4).into(),
    )];
    let invalid = [Op::Set(
        vec!["__proto__".into(), "polluted".into()],
        json!(true).into(),
    )];
    let batches = [&first[..], &invalid[..]];
    let mut visits = 0;
    let iterator = batches
        .into_iter()
        .inspect(|_| {
            visits += 1;
        })
        .chain(std::iter::from_fn(|| panic!("advanced past invalid batch")));
    assert!(apply_immutable_batches(Some(&base), iterator).is_err());
    assert_eq!(visits, 2);
    assert_eq!(base, json!({"nested":{"value":1}}).into());
    Ok(())
}

#[test]
fn one_immutable_batch_can_fan_out_without_shared_mutable_results() -> Result<(), DeltaError> {
    let payload: JsonValue = json!({"nested":{"value":1}}).into();
    let base: JsonValue = json!({"placed":null}).into();
    let operations = vec![
        Op::Set(vec!["placed".into()], payload.clone()),
        Op::Set(
            vec!["placed".into(), "nested".into(), "value".into()],
            json!(2).into(),
        ),
    ];
    let first = apply_immutable(Some(&base), &operations)?;
    let second = apply_immutable(Some(&base), &operations)?;
    assert_eq!(first, second);
    assert!(!first.same_identity(&second));
    assert!(!first["placed"].same_identity(&second["placed"]));
    assert_eq!(payload, json!({"nested":{"value":1}}).into());
    Ok(())
}

#[test]
fn repeated_writes_keep_the_wide_input_revision_unchanged() -> Result<(), DeltaError> {
    let base = JsonValue::object(
        (0..20_000)
            .map(|i| (format!("field{i}").into(), json!(i).into()))
            .collect(),
    );
    let operations: Vec<Op> = (0..1000)
        .map(|i| Op::Set(vec![Seg::Key(format!("field{i}").into())], json!(-i).into()))
        .collect();
    let result = apply_immutable(Some(&base), &operations)?;
    assert_eq!(result["field999"], json!(-999).into());
    assert_eq!(base["field999"], json!(999).into());
    Ok(())
}
