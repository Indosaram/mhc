use maho_chord::{delta::*, JsonValue};
use serde_json::json;

fn replay(prepared: &Prepared) -> Result<(), DeltaError> {
    assert_eq!(
        &apply_immutable(Some(prepared.base()), prepared.ops())?,
        prepared.value()
    );
    Ok(())
}

#[test]
fn twenty_thousand_queue_mutations_normalize_to_two_splices() -> Result<(), DeltaError> {
    let tracker = track(JsonValue::array(
        (0..10_000).map(|i| json!({"value":i}).into()).collect(),
    ))?;
    let mut change = tracker.begin_change();
    let array = change.state()?;
    for value in 10_000..20_000 {
        array.shift()?;
        array.push(&[json!({"value":value}).into()])?;
    }
    let prepared = change.prepare()?;
    assert!(
        matches!(prepared.ops(), [Op::Splice(_, 0, 10_000, removed), Op::Splice(_, 0, 0, inserted)] if removed.is_empty() && inserted.len() == 10_000)
    );
    replay(&prepared)
}

#[test]
fn held_handles_survive_piece_fragmentation_and_detach_on_removal() -> Result<(), DeltaError> {
    let tracker = track(JsonValue::array(
        (0..2000).map(|i| json!({"value":i}).into()).collect(),
    ))?;
    let mut change = tracker.begin_change();
    let array = change.state()?;
    let held = array.get(1000usize)?.ok_or(DeltaError::Root)?;
    for index in (0isize..1000).step_by(2).rev() {
        array.splice(index, 1, &[])?;
    }
    array.reverse()?;
    held.set("value", &json!(-1).into())?;
    let prepared = change.prepare()?;
    assert_eq!(
        resolve_value(prepared.value(), &[999usize.into(), "value".into()])?,
        &json!(-1).into()
    );
    replay(&prepared)
}

#[test]
fn read_only_array_access_preserves_revision_identity() -> Result<(), DeltaError> {
    let initial: JsonValue = json!({"rows":[{"value":1},{"value":2}]}).into();
    let tracker = track(initial.clone())?;
    let mut change = tracker.begin_change();
    let rows = change.state()?.at(&["rows".into()])?;
    assert_eq!(rows.len()?, 2);
    assert_eq!(
        rows.get(1usize)?.ok_or(DeltaError::Root)?.value()?,
        json!({"value":2}).into()
    );
    let prepared = change.prepare()?;
    assert!(prepared.ops().is_empty());
    assert!(prepared.value().same_identity(&initial));
    Ok(())
}

#[test]
fn dirty_objects_emit_in_mutation_order_within_depth() -> Result<(), Box<dyn std::error::Error>> {
    let tracker = track(json!({"left":{"value":0},"right":{"value":0}}).into())?;
    let mut change = tracker.begin_change();
    let root = change.state()?;
    root.at(&["right".into()])?.set("value", &json!(1).into())?;
    root.at(&["left".into()])?.set("value", &json!(2).into())?;
    let prepared = change.prepare()?;
    assert_eq!(
        serde_json::to_value(prepared.ops())?,
        json!([["s", ["right", "value"], 1], ["s", ["left", "value"], 2]])
    );
    replay(&prepared)?;
    Ok(())
}

#[test]
fn moved_inserted_descendants_fold_into_their_placement() -> Result<(), DeltaError> {
    let tracker = track(json!([]).into())?;
    let mut change = tracker.begin_change();
    let array = change.state()?;
    array.push(&[json!({"id":1,"nested":[1]}).into()])?;
    let held = array
        .get(0usize)?
        .ok_or(DeltaError::Root)?
        .at(&["nested".into()])?;
    array.unshift(&[json!({"id":0,"nested":[]}).into()])?;
    held.push(&[json!(2).into()])?;
    array.reverse()?;
    held.push(&[json!(3).into()])?;
    let prepared = change.prepare()?;
    assert_eq!(
        prepared.value(),
        &json!([{"id":1,"nested":[1,2,3]},{"id":0,"nested":[]}]).into()
    );
    replay(&prepared)
}

#[test]
fn cancelled_reverse_and_restored_overrides_emit_nothing() -> Result<(), DeltaError> {
    let initial: JsonValue = json!([1, null, 3]).into();
    let tracker = track(initial.clone())?;
    let mut change = tracker.begin_change();
    let array = change.state()?;
    array.set(1usize, &json!(7).into())?;
    array.set(1usize, &JsonValue::Null)?;
    array.reverse()?;
    array.reverse()?;
    let prepared = change.prepare()?;
    assert!(prepared.ops().is_empty());
    assert!(prepared.value().same_identity(&initial));
    Ok(())
}

#[test]
fn removed_draft_writes_do_not_change_retained_pieces() -> Result<(), DeltaError> {
    let tracker = track(json!([{ "value": "a" }, { "value": "b" }, { "value": "c" }]).into())?;
    let mut change = tracker.begin_change();
    let array = change.state()?;
    let held = array.get(1usize)?.ok_or(DeltaError::Root)?;
    array.unshift(&[json!({"value":"front"}).into()])?;
    held.set("value", &json!("moved").into())?;
    array.splice(2, 1, &[])?;
    held.set("value", &json!("detached").into())?;
    let prepared = change.prepare()?;
    assert_eq!(
        prepared.value(),
        &json!([{ "value": "front" }, { "value": "a" }, { "value": "c" }]).into()
    );
    replay(&prepared)
}
