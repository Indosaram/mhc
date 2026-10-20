use maho_chord::{delta::*, json::parse, services::wire::*, types::*, JsString};
use serde_json::json;

#[test]
fn front_truncation_preserves_a_split_surrogate_in_checked_and_trusted_replay(
) -> Result<(), Box<dyn std::error::Error>> {
    let base = parse(r#"{"text":"\ud83d\ude00tail"}"#)?;
    let operations = vec![Op::Truncate(vec!["text".into()], 1)];
    let expected = parse(r#"{"text":"\ude00tail"}"#)?;
    assert_eq!(apply_immutable(Some(&base), &operations)?, expected);
    assert_eq!(
        apply_immutable_trusted::apply_immutable_trusted(&base, &operations)?,
        expected
    );
    assert_eq!(base, parse(r#"{"text":"\ud83d\ude00tail"}"#)?);
    Ok(())
}

#[test]
fn append_joins_surrogates_without_replacement() -> Result<(), Box<dyn std::error::Error>> {
    let base = parse(r#"{"text":"\ud83d"}"#)?;
    let operations: Vec<Op> = serde_json::from_str(r#"[["a",["text"],"\ude00"]]"#)?;
    let result = apply_immutable(Some(&base), &operations)?;
    assert_eq!(result, parse(r#"{"text":"\ud83d\ude00"}"#)?);
    assert_eq!(serde_json::to_string(&result)?, "{\"text\":\"😀\"}");
    Ok(())
}

#[test]
fn strings_round_trip_each_isolated_surrogate_and_control_escape(
) -> Result<(), Box<dyn std::error::Error>> {
    for unit in 0xd800..=0xdfff {
        let value = JsonValue::String(JsString::from_utf16(vec![0, 34, 92, unit, 10]));
        let encoded = serde_json::to_string(&value)?;
        assert_eq!(serde_json::from_str::<JsonValue>(&encoded)?, value);
    }
    assert_eq!(
        parse(r#"["\ud800",{"value":"\udfff"}]"#)?,
        JsonValue::array(vec![
            JsonValue::String(JsString::from_utf16(vec![0xd800])),
            JsonValue::object(vec![(
                "value".into(),
                JsonValue::String(JsString::from_utf16(vec![0xdfff]))
            )]),
        ])
    );
    Ok(())
}

#[test]
fn diff_and_tracker_emit_lossless_string_suffixes() -> Result<(), Box<dyn std::error::Error>> {
    let before = parse(r#"{"text":"\ud800abc"}"#)?;
    let after = parse(r#"{"text":"abc\udfff"}"#)?;
    let operations = diff_revisions(&before, &after);
    assert_eq!(
        operations,
        vec![
            Op::Truncate(vec!["text".into()], 1),
            Op::Append(vec!["text".into()], JsString::from_utf16(vec![0xdfff]))
        ]
    );
    let tracker = track(before.clone())?;
    let mut change = tracker.begin_change();
    change
        .state()?
        .set("text", resolve_value(&after, &["text".into()])?)?;
    assert_eq!(change.prepare()?.ops(), operations);
    assert_eq!(apply_immutable(Some(&before), &operations)?, after);
    Ok(())
}

#[test]
fn codec_interning_and_service_wire_keep_surrogates_through_json_text() -> Result<(), ServiceError>
{
    let operations: Vec<Op> = serde_json::from_str(
        r#"[["r",{"text":"\ud800"}],["a",["text"],"\udfff"],["a",["text"],"\ud800"]]"#,
    )?;
    let wire = encoder().encode(&operations);
    let text = serde_json::to_string(&wire)?;
    let decoded_wire: Vec<WireOp> = serde_json::from_str(&text)?;
    assert_eq!(decoder().decode(&decoded_wire)?, operations);
    let update = WireServiceProviderUpdate::State {
        instance: None,
        member: "state".into(),
        sequence: 1,
        ops: wire,
    };
    let parsed = parse_wire_service_provider_update(parse(&serde_json::to_string(&update)?)?)?;
    let WireServiceProviderUpdate::State { ops, .. } = parsed else {
        return Err("expected state".into());
    };
    assert_eq!(decoder().decode(&ops)?, operations);
    assert!(serde_json::to_value(JsonValue::String(JsString::from_utf16(vec![0xd800]))).is_err());
    assert!(parse_service_call(json!({"serviceId":"x","member":"m","args":[]})).is_ok());
    Ok(())
}

#[test]
fn service_snapshot_and_call_arguments_keep_surrogate_payloads() -> Result<(), ServiceError> {
    let snapshot = parse_wire_service_subscription_snapshot(parse(
        r#"{"serviceId":"test","mode":"singleton","instances":[{"members":[{"kind":"state","name":"state","sequence":0,"ops":[["r",{"text":"\ud800"}]]}]}]}"#,
    )?)?;
    let text = serde_json::to_string(&snapshot)?;
    let snapshot: WireServiceSubscriptionSnapshot = serde_json::from_str(&text)?;
    let mut codec = maho_chord::create_service_state_decoder();
    let decoded = codec.decode_snapshot(snapshot)?;
    let ServiceMemberSnapshot::State { ops, .. } = &decoded.instances[0].members[0] else {
        return Err("expected state".into());
    };
    assert_eq!(ops, &vec![Op::Replace(parse(r#"{"text":"\ud800"}"#)?)]);
    let call = parse_service_call(parse(
        r#"{"serviceId":"test","member":"method","args":["\udfff"]}"#,
    )?)?;
    assert_eq!(
        call.args,
        vec![JsonValue::String(JsString::from_utf16(vec![0xdfff]))]
    );
    assert_eq!(
        serde_json::from_str::<ServiceCall>(&serde_json::to_string(&call)?)?.args,
        call.args
    );
    Ok(())
}
