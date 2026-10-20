use maho_chord::{delta::*, JsonValue};
use serde_json::json;
use std::sync::{Arc, Weak};

fn weak_rows(value: &JsonValue) -> Result<Weak<Vec<JsonValue>>, DeltaError> {
    match &value["rows"] {
        JsonValue::Array(rows) => Ok(Arc::downgrade(rows)),
        _ => Err(DeltaError::Root),
    }
}
fn initial() -> JsonValue {
    json!({"rows":[{"value":0},{"value":1}],"child":{"value":0}}).into()
}

#[test]
fn settled_change_and_held_draft_release_obsolete_revisions() -> Result<(), DeltaError> {
    let mut tracker = track(initial())?;
    let obsolete = weak_rows(tracker.value())?;
    let mut change = tracker.begin_change();
    let held = change.state()?.at(&["rows".into(), 0usize.into()])?;
    held.set("value", &json!(-1).into())?;
    let prepared = change.prepare()?;
    tracker.adopt(&prepared)?;
    drop(prepared);
    let replacement = tracker.prepare_replace(json!({"rows":[],"child":{"value":2}}).into())?;
    tracker.adopt(&replacement)?;
    drop(replacement);
    assert!(obsolete.upgrade().is_none());
    assert!(held.value().is_err());
    assert!(change.prepare().is_err());
    Ok(())
}

#[test]
fn retained_prepared_keeps_base_and_candidate_until_dropped() -> Result<(), DeltaError> {
    let mut tracker = track(initial())?;
    let base = weak_rows(tracker.value())?;
    let mut change = tracker.begin_change();
    change
        .state()?
        .at(&["rows".into(), 0usize.into()])?
        .set("value", &json!(-1).into())?;
    let prepared = change.prepare()?;
    let candidate = weak_rows(prepared.value())?;
    tracker.adopt(&prepared)?;
    let replacement = tracker.prepare_replace(json!({"rows":[]}).into())?;
    tracker.adopt(&replacement)?;
    assert!(base.upgrade().is_some());
    assert!(candidate.upgrade().is_some());
    drop(prepared);
    drop(replacement);
    assert!(base.upgrade().is_none());
    assert!(candidate.upgrade().is_none());
    Ok(())
}

#[test]
fn stale_open_overlay_does_not_retain_its_old_root() -> Result<(), DeltaError> {
    let mut tracker = track(initial())?;
    let obsolete = weak_rows(tracker.value())?;
    let mut stale = tracker.begin_change();
    let held = stale.state()?.at(&["child".into()])?;
    held.set("value", &json!(-1).into())?;
    let replacement = tracker.prepare_replace(json!({"rows":[]}).into())?;
    tracker.adopt(&replacement)?;
    drop(replacement);
    assert!(obsolete.upgrade().is_none());
    assert!(stale.prepare().is_err());
    assert!(held.value().is_err());
    Ok(())
}

#[test]
fn aborted_payload_and_unadopted_prepared_release_without_collection() -> Result<(), DeltaError> {
    let tracker = track(json!({"rows":[]}).into())?;
    let mut change = tracker.begin_change();
    change.state()?.set("rows", &json!([1, 2, 3]).into())?;
    let held = change.state()?.at(&["rows".into()])?;
    let payload = held.value()?;
    let JsonValue::Array(rows) = payload else {
        return Err(DeltaError::Root);
    };
    let weak = Arc::downgrade(&rows);
    drop(rows);
    change.abort();
    assert!(weak.upgrade().is_none());
    assert!(held.value().is_err());
    let mut change = tracker.begin_change();
    change.state()?.set("rows", &json!([4, 5, 6]).into())?;
    let prepared = change.prepare()?;
    let weak = weak_rows(prepared.value())?;
    drop(prepared);
    assert!(weak.upgrade().is_none());
    Ok(())
}

#[test]
fn lifecycle_churn_leaves_authority_usable() -> Result<(), DeltaError> {
    let mut tracker = track(json!({"value":0}).into())?;
    for value in 0..100_000 {
        let mut change = tracker.begin_change();
        change.state()?.set("value", &json!(value).into())?;
        change.abort();
    }
    let prepared = tracker.prepare_replace(json!({"value":1}).into())?;
    tracker.adopt(&prepared)?;
    assert_eq!(tracker.value(), &json!({"value":1}).into());
    Ok(())
}

#[test]
fn abort_after_prepare_and_retained_placements_keep_only_their_owned_values(
) -> Result<(), DeltaError> {
    let tracker = track(initial())?;
    let mut change = tracker.begin_change();
    let held = change.state()?.at(&["rows".into()])?;
    held.push(&[json!({"value":2}).into()])?;
    let prepared = change.prepare()?;
    let candidate = weak_rows(prepared.value())?;
    change.abort();
    assert!(held.value().is_err());
    assert!(candidate.upgrade().is_some());
    assert_eq!(prepared.value()["rows"].as_array().map(Vec::len), Some(3));
    drop(prepared);
    assert!(candidate.upgrade().is_none());
    Ok(())
}

#[test]
fn repeated_revisions_do_not_accumulate_accessed_or_folded_overlay_ownership(
) -> Result<(), DeltaError> {
    let mut tracker = track(JsonValue::object(vec![(
        "rows".into(),
        JsonValue::array(
            (0..5000)
                .map(|value| json!({"value":value}).into())
                .collect(),
        ),
    )]))?;
    let mut old = Vec::new();
    for value in 0..100 {
        old.push(weak_rows(tracker.value())?);
        let mut change = tracker.begin_change();
        let rows = change.state()?.at(&["rows".into()])?;
        for index in 0..4094 {
            rows.get(index)?.ok_or(DeltaError::Root)?.value()?;
        }
        rows.get(4999usize)?
            .ok_or(DeltaError::Root)?
            .set("value", &json!(-value - 1).into())?;
        let prepared = change.prepare()?;
        tracker.adopt(&prepared)?;
    }
    assert!(old.iter().all(|value| value.upgrade().is_none()));
    let mut tracker = track(JsonValue::object(
        (0..5000)
            .map(|i| (format!("field{i}").into(), json!(0).into()))
            .collect(),
    ))?;
    let mut old = Vec::new();
    for value in 1..=20 {
        let JsonValue::Object(root) = tracker.value() else {
            return Err(DeltaError::Root);
        };
        old.push(Arc::downgrade(root));
        let mut change = tracker.begin_change();
        let root = change.state()?;
        for index in 0..5000 {
            root.set(Seg::Key(format!("field{index}").into()), &json!(value).into())?;
        }
        let prepared = change.prepare()?;
        assert!(matches!(prepared.ops(), [Op::Replace(_)]));
        tracker.adopt(&prepared)?;
    }
    assert!(old.iter().all(|value| value.upgrade().is_none()));
    Ok(())
}
