use maho_chord::{
    copy_json,
    delta::{resolve_value, revision_validator::JsonRevisionValidator},
    is_json_value, CopyJsonOptions, JsonValue,
};
use serde_json::json;

#[test]
fn copy_detaches_repeated_container_aliases() {
    let shared: JsonValue = json!({"value":1}).into();
    let input = JsonValue::object(vec![
        ("left".into(), shared.clone()),
        ("right".into(), shared.clone()),
    ]);
    let copied = copy_json(&input, CopyJsonOptions::default());
    let left = resolve_value(&copied, &["left".into()]).expect("left");
    let right = resolve_value(&copied, &["right".into()]).expect("right");
    assert_eq!(copied, input);
    assert!(!left.same_identity(&shared));
    assert!(!right.same_identity(&shared));
    assert!(!left.same_identity(right));
}
#[test]
fn reserved_keys_remain_own_json_data() {
    let input: JsonValue = json!({"__proto__":{"safe":true}}).into();
    let copied = copy_json(
        &input,
        CopyJsonOptions {
            omit_undefined_properties: true,
        },
    );
    assert!(is_json_value(&copied));
    assert_eq!(
        serde_json::to_value(&copied).expect("serialize"),
        json!({"__proto__":{"safe":true}})
    );
}
#[test]
fn deserialization_refuses_nonfinite_and_nonjson_inputs() {
    for input in [
        "NaN",
        "Infinity",
        "undefined",
        "[1,,3]",
        "{\"x\":undefined}",
    ] {
        assert!(serde_json::from_str::<JsonValue>(input).is_err());
    }
}
#[test]
fn validator_caches_shared_containers_without_retaining_revisions() {
    let shared: JsonValue = json!({"nested":[1,2]}).into();
    let mut validator = JsonRevisionValidator::new();
    validator.validate(&shared);
    let count = validator.cached_containers();
    validator.validate(&shared);
    assert_eq!(validator.cached_containers(), count);
    drop(shared);
    validator.validate(&JsonValue::Null);
    assert_eq!(validator.cached_containers(), 0);
}

#[test]
fn object_keys_preserve_isolated_surrogates_and_duplicate_positions() -> Result<(), serde_json::Error> {
    let input = r#"{"\ud800":1,"a":{"\udfff":["},:\"",2]},"\ud800":3,"2":4}"#;
    let value = maho_chord::json::parse(input)?;
    let copied = copy_json(&value, CopyJsonOptions::default());
    let text = serde_json::to_string(&copied)?;
    assert_eq!(maho_chord::json::parse(&text)?, value);
    assert_eq!(serde_json::from_str::<JsonValue>(&text)?, value);
    let keys: Vec<_> = value.as_object().expect("object").iter().map(|(key, _)| key.units()).collect();
    assert_eq!(keys, vec![vec![50], vec![0xd800], vec![97]]);
    assert_eq!(value.get_key(&maho_chord::JsString::from_utf16(vec![0xd800])), Some(&json!(3).into()));
    assert!(serde_json::Value::try_from(&value).is_err());
    Ok(())
}

#[test]
fn every_isolated_surrogate_round_trips_as_an_object_key() -> Result<(), serde_json::Error> {
    for unit in 0xd800..=0xdfff {
        let key = maho_chord::JsString::from_utf16(vec![0, 34, 92, unit, 10]);
        let value = JsonValue::object(vec![(key, json!(unit).into())]);
        let text = serde_json::to_string(&value)?;
        assert_eq!(serde_json::from_str::<JsonValue>(&text)?, value);
    }
    Ok(())
}
