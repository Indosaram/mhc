use maho_chord::{delta::*, JsonValue};
use serde_json::json;
fn v(value: serde_json::Value) -> JsonValue {
    value.into()
}
fn replay(prepared: &Prepared) {
    assert_eq!(
        &apply_immutable(Some(prepared.base()), prepared.ops()).expect("replay"),
        prepared.value()
    );
}

#[test]
fn prepare_materializes_without_adopting_and_revokes_held_drafts() {
    let initial = v(json!({"count":1,"nested":{"text":"a"},"values":[1]}));
    let mut tracker = track(initial.clone()).expect("track");
    let mut change = tracker.begin_change();
    let draft = change.state().expect("draft");
    draft.set("count", &v(json!(2))).expect("set");
    draft
        .at(&["nested".into()])
        .expect("nested")
        .set("text", &v(json!("ab")))
        .expect("set");
    draft
        .at(&["values".into()])
        .expect("array")
        .push(&[v(json!(2))])
        .expect("push");
    let prepared = change.prepare().expect("prepare");
    assert_eq!(tracker.value(), &initial);
    assert!(draft.value().is_err());
    replay(&prepared);
    assert_eq!(
        serde_json::to_value(prepared.ops()).expect("serialize"),
        json!([
            ["s", ["count"], 2],
            ["a", ["nested", "text"], "b"],
            ["p", ["values"], 1, 0, [2]]
        ])
    );
    tracker.adopt(&prepared).expect("adopt");
    assert!(tracker.value().same_identity(prepared.value()));
}
#[test]
fn competing_noop_adoption_invalidates_open_and_prepared_views() {
    let mut tracker = track(v(json!({"value":1}))).expect("track");
    let mut open = tracker.begin_change();
    let held = open.state().expect("draft");
    let loser = tracker
        .prepare_replace(v(json!({"value":2})))
        .expect("loser");
    let winner = tracker
        .prepare_replace(v(json!({"value":1})))
        .expect("winner");
    tracker.adopt(&winner).expect("adopt");
    assert!(held.value().is_err());
    assert!(open.prepare().is_err());
    assert_eq!(tracker.adopt(&loser), Err(DeltaError::Stale));
    assert_eq!(tracker.adopt(&winner), Err(DeltaError::Consumed));
    assert_eq!(loser.value(), &v(json!({"value":2})));
}
#[test]
fn foreign_and_aborted_preparations_cannot_be_adopted() {
    let mut first = track(v(json!({}))).expect("track");
    let mut second = track(v(json!({}))).expect("track");
    let prepared = first
        .prepare_replace(v(json!({"value":1})))
        .expect("prepare");
    assert_eq!(second.adopt(&prepared), Err(DeltaError::Foreign));
    prepared.abort();
    prepared.abort();
    assert_eq!(first.adopt(&prepared), Err(DeltaError::Aborted));
}
#[test]
fn aborting_settled_change_aborts_its_preparation() {
    let mut tracker = track(v(json!({"value":0}))).expect("track");
    let mut change = tracker.begin_change();
    change
        .state()
        .expect("draft")
        .set("value", &v(json!(1)))
        .expect("set");
    let prepared = change.prepare().expect("prepare");
    change.abort();
    assert_eq!(tracker.adopt(&prepared), Err(DeltaError::Aborted));
    assert_eq!(prepared.value(), &v(json!({"value":1})));
}
#[test]
fn equal_assignment_detaches_old_handles_but_preserves_committed_identity() {
    let initial = v(json!({"object":{"value":1}}));
    let tracker = track(initial.clone()).expect("track");
    let mut change = tracker.begin_change();
    let root = change.state().expect("draft");
    let held = root.at(&["object".into()]).expect("held");
    root.set("object", &v(json!({"value":1}))).expect("set");
    held.set("value", &v(json!(9))).expect("detached write");
    let prepared = change.prepare().expect("prepare");
    assert!(prepared.ops().is_empty());
    assert!(prepared.value().same_identity(&initial));
}
#[test]
fn moved_held_descendant_emits_final_array_path() {
    let tracker = track(v(
        json!({"values":[{"rank":3,"edited":0},{"rank":1,"edited":0},{"rank":2,"edited":0}]}),
    ))
    .expect("track");
    let mut change = tracker.begin_change();
    let array = change
        .state()
        .expect("draft")
        .at(&["values".into()])
        .expect("array");
    let held = array.get(0usize).expect("get").expect("row");
    array.reverse().expect("reverse");
    held.set("edited", &v(json!(1))).expect("write");
    let prepared = change.prepare().expect("prepare");
    replay(&prepared);
    assert_eq!(
        serde_json::to_value(prepared.ops()).expect("serialize"),
        json!([
            ["m", ["values"], [2, 1, 0]],
            ["s", ["values", 2, "edited"], 1]
        ])
    );
}
#[test]
fn introduced_subtree_edits_fold_into_placement() {
    let tracker = track(v(json!({"values":[]}))).expect("track");
    let mut change = tracker.begin_change();
    let array = change
        .state()
        .expect("draft")
        .at(&["values".into()])
        .expect("array");
    array.push(&[v(json!({"value":1}))]).expect("push");
    array
        .get(0usize)
        .expect("get")
        .expect("row")
        .set("value", &v(json!(2)))
        .expect("set");
    let prepared = change.prepare().expect("prepare");
    replay(&prepared);
    assert_eq!(
        serde_json::to_value(prepared.ops()).expect("serialize"),
        json!([["p",["values"],0,0,[{"value":2}]]])
    );
}
#[test]
fn placement_from_draft_is_detached_by_value() {
    let tracker = track(v(json!({"left":{"value":1},"right":null}))).expect("track");
    let mut change = tracker.begin_change();
    let root = change.state().expect("draft");
    let left = root.at(&["left".into()]).expect("left");
    root.place("right", &left).expect("place");
    left.set("value", &v(json!(9))).expect("set");
    let prepared = change.prepare().expect("prepare");
    replay(&prepared);
    assert_eq!(
        prepared.value(),
        &v(json!({"left":{"value":9},"right":{"value":1}}))
    );
}
#[test]
fn structural_mutators_and_null_growth_replay_exactly() {
    let tracker = track(v(json!([1, 2, 3, 4]))).expect("track");
    let mut change = tracker.begin_change();
    let array = change.state().expect("array");
    array.copy_within(1, 0, Some(3)).expect("copy");
    array.fill(&v(json!(7)), 2, None).expect("fill");
    array.unshift(&[v(json!(0))]).expect("unshift");
    array.pop().expect("pop");
    array.set_length(6).expect("grow");
    let prepared = change.prepare().expect("prepare");
    replay(&prepared);
    assert_eq!(prepared.value(), &v(json!([0, 1, 1, 7, null, null])));
}
#[test]
fn dense_edits_emit_one_region_and_keep_outer_fields_narrow() {
    let rows: Vec<JsonValue> = (0..600).map(|i| v(json!({"value":i}))).collect();
    let tracker = track(JsonValue::array(rows)).expect("track");
    let mut change = tracker.begin_change();
    let array = change.state().expect("array");
    for i in 0..600 {
        array
            .get(i)
            .expect("get")
            .expect("row")
            .set("value", &v(json!(-1)))
            .expect("set");
    }
    let prepared = change.prepare().expect("prepare");
    replay(&prepared);
    assert!(matches!(prepared.ops(),[Op::Splice(path,0,600,_)] if path.is_empty()));
}
#[test]
fn reserved_own_keys_fold_without_unsafe_paths() {
    let tracker = track(v(json!({"object":{"constructor":{"value":1}}}))).expect("track");
    let mut change = tracker.begin_change();
    change
        .state()
        .expect("draft")
        .at(&["object".into(), "constructor".into()])
        .expect("own data")
        .set("value", &v(json!(2)))
        .expect("set");
    let prepared = change.prepare().expect("prepare");
    replay(&prepared);
    assert_eq!(
        serde_json::to_value(prepared.ops()).expect("serialize"),
        json!([["s",["object"],{"constructor":{"value":2}}]])
    );
}
#[test]
fn delete_and_readd_preserves_string_insertion_order() {
    let tracker = track(v(json!({"a":1,"b":2}))).expect("track");
    let mut change = tracker.begin_change();
    let draft = change.state().expect("draft");
    draft.delete("a").expect("delete");
    draft.set("a", &v(json!(1))).expect("set");
    let prepared = change.prepare().expect("prepare");
    assert_eq!(
        serde_json::to_string(prepared.value()).expect("serialize"),
        "{\"b\":2,\"a\":1}"
    );
}
