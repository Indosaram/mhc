use maho_chord::{delta::*, json::parse, JsString, JsonValue};
use serde_json::json;

#[test]
fn surrogate_paths_survive_diff_tracker_and_interned_wire() -> Result<(), Box<dyn std::error::Error>> {
    let base = parse(r#"{"\ud800":{"\udfff":"start"},"other":0}"#)?;
    let after = parse(r#"{"\ud800":{"\udfff":"start!"},"other":1}"#)?;
    let operations = diff_revisions(&base, &after);
    let mut encode = encoder();
    let mut decode = decoder();
    for _ in 0..3 {
        let wire = encode.encode(&operations);
        let text = serde_json::to_string(&wire)?;
        let wire: Vec<WireOp> = serde_json::from_str(&text)?;
        assert_eq!(decode.decode(&wire)?, operations);
    }
    let tracker = track(base.clone())?;
    let mut change = tracker.begin_change();
    let root = change.state()?;
    root.at(&[JsString::from_utf16(vec![0xd800]).into()])?
        .set(JsString::from_utf16(vec![0xdfff]), &json!("start!").into())?;
    root.set("other", &json!(1).into())?;
    let prepared = change.prepare()?;
    assert_eq!(prepared.value(), &after);
    assert_eq!(apply_immutable(Some(&base), prepared.ops())?, after);
    let text = serde_json::to_string(prepared.ops())?;
    assert_eq!(serde_json::from_str::<Vec<Op>>(&text)?, prepared.ops());
    let segment = Seg::Key(JsString::from_utf16(vec![0xd800]));
    assert_eq!(serde_json::from_str::<Seg>(&serde_json::to_string(&segment)?)?, segment);
    Ok(())
}

#[test]
fn surrogate_and_replacement_character_keys_never_alias() -> Result<(), Box<dyn std::error::Error>> {
    let base = parse(r#"{"\ud800":1,"\ufffd":2,"constructorX":3}"#)?;
    let tracker = track(base.clone())?;
    let mut change = tracker.begin_change();
    let root = change.state()?;
    root.delete(JsString::from_utf16(vec![0xd800]))?;
    root.set(JsString::from_utf16(vec![0xd800]), &json!(9).into())?;
    assert_eq!(root.keys()?.last(), Some(&JsString::from_utf16(vec![0xd800])));
    let prepared = change.prepare()?;
    assert_eq!(prepared.value()["\u{fffd}"], json!(2).into());
    assert_eq!(prepared.value().get_key(&JsString::from_utf16(vec![0xd800])), Some(&json!(9).into()));
    assert_eq!(&apply_immutable(Some(&base), prepared.ops())?, prepared.value());
    Ok(())
}

#[test]
fn fragmented_source_locations_are_invalidated_after_each_structural_edit() -> Result<(), DeltaError> {
    let tracker = track(JsonValue::array((0..3000).map(|i| json!({"value":i}).into()).collect()))?;
    let mut change = tracker.begin_change();
    let array = change.state()?;
    let held = array.get(2500usize)?.ok_or(DeltaError::Root)?;
    for i in (0..1000).rev() {
        array.splice(i * 2, 0, &[JsonValue::Null])?;
    }
    held.set("value", &json!(-1).into())?;
    array.reverse()?;
    array.splice(0, 2, &[])?;
    let expected = array.value()?;
    let prepared = change.prepare()?;
    assert_eq!(prepared.value(), &expected);
    assert_eq!(prepared.value()[497]["value"], json!(-1).into());
    assert_eq!(&apply_immutable(Some(prepared.base()), prepared.ops())?, prepared.value());
    Ok(())
}

#[test]
fn deleting_readding_and_replacing_a_held_object_key_detaches_old_handles() -> Result<(), Box<dyn std::error::Error>> {
    let base = parse(r#"{"\ud800":{"value":0},"stable":{"value":1},"tail":2}"#)?;
    let tracker = track(base.clone())?;
    let mut change = tracker.begin_change();
    let root = change.state()?;
    let key = JsString::from_utf16(vec![0xd800]);
    let held = root.get(key.clone())?.ok_or(DeltaError::Root)?;
    root.delete(key.clone())?;
    held.set("value", &json!(100).into())?;
    root.set(key.clone(), &json!({"value":2}).into())?;
    held.set("value", &json!(200).into())?;
    root.get(key)?.ok_or(DeltaError::Root)?.set("value", &json!(3).into())?;
    let prepared = change.prepare()?;
    assert_eq!(prepared.value(), &parse(r#"{"stable":{"value":1},"tail":2,"\ud800":{"value":3}}"#)?);
    assert!(prepared.value()["stable"].same_identity(&base["stable"]));
    assert_eq!(&apply_immutable(Some(&base), prepared.ops())?, prepared.value());
    Ok(())
}

#[test]
fn cached_locations_drop_removed_sources_and_track_later_reverse() -> Result<(), DeltaError> {
    let tracker = track(json!([{"value":0},{"value":1},{"value":2}]).into())?;
    let mut change = tracker.begin_change();
    let array = change.state()?;
    let removed = array.get(1usize)?.ok_or(DeltaError::Root)?;
    let retained = array.get(2usize)?.ok_or(DeltaError::Root)?;
    array.reverse()?;
    array.splice(1, 1, &[])?;
    removed.set("value", &json!(99).into())?;
    retained.set("value", &json!(5).into())?;
    array.reverse()?;
    let prepared = change.prepare()?;
    assert_eq!(prepared.value(), &json!([{"value":0},{"value":5}]).into());
    assert_eq!(&apply_immutable(Some(prepared.base()), prepared.ops())?, prepared.value());
    Ok(())
}
