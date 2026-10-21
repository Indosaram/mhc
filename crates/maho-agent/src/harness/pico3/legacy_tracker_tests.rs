use super::*;
use maho_chord::delta::{apply_immutable, is_base};

fn json(source: &str) -> JsonValue { serde_json::from_str(source).expect("JSON fixture") }

#[test]
fn first_flush_rebase_and_empty_changes_match_legacy_contract() {
    let mut tracker = track(json(r#"{"value":1}"#)).expect("track");
    assert!(tracker.dirty());
    let first = tracker.flush().expect("flush");
    assert!(is_base(&first));
    assert!(!tracker.dirty());
    assert!(tracker.flush().expect("clean flush").is_empty());
    let untouched = tracker.state().expect("state");
    assert!(tracker.dirty());
    assert!(tracker.flush().expect("untouched flush").is_empty());
    assert!(untouched.set("value", &json("2")).is_err());
    tracker.rebase();
    assert!(tracker.dirty());
    assert!(is_base(&tracker.flush().expect("rebase")));
}

#[test]
fn descendant_copies_remain_independent_and_flush_adopts_before_storage() {
    let mut tracker = track(json(r#"{"a":{"child":{"value":1}},"b":null,"rows":[{"child":{"value":1}},{"child":{"value":2}}]}"#)).expect("track");
    let mut replica = apply_immutable(None, &tracker.flush().expect("base")).expect("apply");
    let draft = tracker.state().expect("state");
    draft.at(&["a".into(), "child".into()]).expect("child").set("value", &json("2")).expect("set");
    let a = draft.at(&["a".into()]).expect("a").value().expect("snapshot");
    draft.set("b", &a).expect("copy a");
    let row = draft.at(&["rows".into(), 0_usize.into()]).expect("row").value().expect("snapshot");
    draft.at(&["rows".into()]).expect("rows").set(1_usize, &row).expect("copy row");
    draft.at(&["rows".into(), 1_usize.into(), "child".into()]).expect("child").set("value", &json("3")).expect("set");
    let operations = tracker.flush().expect("flush");
    assert_eq!(tracker.target(), json(r#"{"a":{"child":{"value":2}},"b":{"child":{"value":2}},"rows":[{"child":{"value":1}},{"child":{"value":3}}]}"#));
    replica = apply_immutable(Some(&replica), &operations).expect("replicate");
    assert_eq!(replica, tracker.target());
    assert!(!tracker.dirty());
}

#[test]
fn surrogate_keys_and_strings_survive_tracking_and_replay() {
    let original = json(r#"{"\ud800":"\udfff"}"#);
    let mut tracker = track(original.clone()).expect("track");
    let base = tracker.flush().expect("flush");
    assert_eq!(apply_immutable(None, &base).expect("replay"), original);
    let wire = serde_json::to_string(&base).expect("serialize");
    let recovered: Vec<Op> = serde_json::from_str(&wire).expect("deserialize");
    assert_eq!(apply_immutable(None, &recovered).expect("replay"), original);
}
