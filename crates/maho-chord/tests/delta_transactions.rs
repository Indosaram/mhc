use maho_chord::{delta::*, JsonValue};
use serde_json::json;
fn v(value: serde_json::Value) -> JsonValue {
    value.into()
}

#[test]
fn deterministic_multi_operation_transactions_match_model_and_replay() {
    for seed in 1u64..=40 {
        let mut random = seed;
        let mut model = vec![0i64, 1, 2, 3];
        let mut tracker = track(v(json!(model))).expect("track");
        for transaction in 0..25 {
            let before = tracker.value().clone();
            let mut change = tracker.begin_change();
            let draft = change.state().expect("draft");
            for operation in 0..5 {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                let choice = (random >> 32) % 7;
                let number =
                    i64::try_from(seed).expect("small") * 10_000 + transaction * 10 + operation;
                match choice {
                    0 => {
                        model.push(number);
                        draft.push(&[v(json!(number))]).expect("push");
                    }
                    1 => {
                        if !model.is_empty() {
                            model.remove(0);
                        }
                        draft.shift().expect("shift");
                    }
                    2 => {
                        model.reverse();
                        draft.reverse().expect("reverse");
                    }
                    3 => {
                        model.insert(0, number);
                        draft.unshift(&[v(json!(number))]).expect("unshift");
                    }
                    4 => {
                        model.pop();
                        draft.pop().expect("pop");
                    }
                    5 => {
                        if !model.is_empty() {
                            model[0] = number;
                            draft.set(0usize, &v(json!(number))).expect("set");
                        }
                    }
                    6 => {
                        model.sort_by_key(|value| value.to_string());
                        draft.sort().expect("sort");
                    }
                    _ => unreachable!(),
                }
            }
            let expected = v(json!(model));
            assert_eq!(draft.value().expect("policy"), expected);
            let prepared = change.prepare().expect("prepare");
            assert_eq!(tracker.value(), &before);
            assert_eq!(prepared.value(), &expected);
            assert_eq!(
                apply_immutable(Some(&before), prepared.ops()).expect("replay"),
                expected
            );
            tracker.adopt(&prepared).expect("adopt");
            assert_eq!(tracker.value(), &expected);
        }
    }
}

#[test]
fn large_queue_normalizes_many_changes_to_two_splices() {
    let initial: Vec<JsonValue> = (0..1200).map(|i| v(json!({"value":i}))).collect();
    let tracker = track(JsonValue::array(initial)).expect("track");
    let mut change = tracker.begin_change();
    let array = change.state().expect("array");
    for i in 0..600 {
        array.shift().expect("shift");
        array.push(&[v(json!({"value":1200+i}))]).expect("push");
    }
    let prepared = change.prepare().expect("prepare");
    assert_eq!(prepared.ops().len(), 2);
    assert!(
        matches!(&prepared.ops()[0],Op::Splice(path,0,600,items) if path.is_empty() && items.is_empty())
    );
    assert!(
        matches!(&prepared.ops()[1],Op::Splice(path,600,0,items) if path.is_empty() && items.len() == 600)
    );
    assert_eq!(
        &apply_immutable(Some(prepared.base()), prepared.ops()).expect("replay"),
        prepared.value()
    );
}

#[test]
fn integer_object_keys_precede_strings_in_native_key_order() {
    let tracker = track(v(json!({"z":0}))).expect("track");
    let mut change = tracker.begin_change();
    let draft = change.state().expect("draft");
    for key in ["9", "2", "a", "01"] {
        draft.set(key, &v(json!(1))).expect("set");
    }
    assert_eq!(draft.keys().expect("keys"), vec!["2", "9", "z", "a", "01"]);
    let prepared = change.prepare().expect("prepare");
    assert_eq!(
        serde_json::to_string(prepared.value()).expect("serialize"),
        "{\"2\":1,\"9\":1,\"z\":0,\"a\":1,\"01\":1}"
    );
}
