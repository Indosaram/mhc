use maho_chord::{
    delta::Op,
    services::{state_codec::*, wire::*},
    types::*,
};
use serde_json::json;

#[test]
fn codecs_use_independent_state_dictionaries_and_exact_short_forms() -> Result<(), ServiceError> {
    let mut encoder = create_service_state_encoder();
    let mut decoder = create_service_state_decoder();
    let mut baseline = snapshot();
    baseline.instances[0]
        .members
        .push(ServiceMemberSnapshot::State {
            name: "other".into(),
            sequence: seq(0),
            ops: vec![Op::Replace(json!({"revision":0}).into())],
        });
    decoder.decode_snapshot(encoder.encode_snapshot(baseline.clone())?)?;
    for (member, sequence, expected) in [
        ("state", 1, json!([["s", ["revision"], 1]])),
        ("state", 2, json!([["#", 0, ["revision"]], ["s", 0, 2]])),
        ("other", 1, json!([["s", ["revision"], 1]])),
    ] {
        let update = ServiceProviderUpdate::State {
            instance: None,
            member: member.into(),
            sequence: seq(sequence),
            ops: vec![Op::Set(vec!["revision".into()], json!(sequence).into())],
        };
        let wire = encoder.encode_update(update.clone())?;
        if let WireServiceProviderUpdate::State { ops, .. } = &wire {
            assert_eq!(json!(ops), expected);
        } else {
            return Err("expected state update".into());
        }
        assert_eq!(
            serde_json::to_value(decoder.decode_update(wire)?)?,
            serde_json::to_value(update)?
        );
    }
    decoder.decode_update(
        encoder.encode_update(ServiceProviderUpdate::Reset { snapshot: baseline })?,
    )?;
    let wire = encoder.encode_update(ServiceProviderUpdate::State {
        instance: None,
        member: "state".into(),
        sequence: seq(1),
        ops: vec![Op::Set(vec!["revision".into()], json!(1).into())],
    })?;
    if let WireServiceProviderUpdate::State { ops, .. } = wire {
        assert_eq!(json!(ops), json!([["s", ["revision"], 1]]));
    } else {
        return Err("expected update after reset".into());
    }
    Ok(())
}

fn snapshot() -> ServiceSubscriptionSnapshot {
    ServiceSubscriptionSnapshot {
        service_id: "test.models".into(),
        mode: ServiceMode::Singleton,
        instances: vec![ServiceInstanceSnapshot {
            instance: None,
            members: vec![ServiceMemberSnapshot::State {
                name: "state".into(),
                sequence: seq(0),
                ops: vec![Op::Replace(json!({"revision":0}).into())],
            }],
        }],
    }
}

#[test]
fn wire_sequences_follow_js_number_rounding_not_safe_integer_restrictions() -> Result<(), ServiceError> {
    let update = maho_chord::json::parse(r#"{"type":"state","member":"state","sequence":9007199254740993,"ops":[]}"#)?;
    let parsed = parse_service_provider_update(update)?;
    assert!(matches!(parsed, ServiceProviderUpdate::State { sequence, .. } if sequence == seq(9_007_199_254_740_992)));
    for sequence in ["1.0", "1e3", "-0"] {
        let input = maho_chord::json::parse(&format!(r#"{{"serviceId":"test","mode":"singleton","instances":[{{"members":[{{"name":"state","kind":"state","sequence":{sequence},"ops":[["r",{{}}]]}}]}}]}}"#))?;
        parse_service_subscription_snapshot(input)?;
    }
    for sequence in ["-1", "1.5"] {
        let input = maho_chord::json::parse(&format!(r#"{{"type":"state","member":"state","sequence":{sequence},"ops":[]}}"#))?;
        assert!(parse_service_provider_update(input).is_err());
    }
    Ok(())
}

#[test]
fn sequences_cover_finite_js_numbers_beyond_u64_and_preserve_increment() -> Result<(), ServiceError> {
    for token in ["18446744073709551616", "1e100", "1.7976931348623157e308"] {
        let input = maho_chord::json::parse(&format!(r#"{{"type":"state","member":"state","sequence":{token},"ops":[]}}"#))?;
        let update = parse_service_provider_update(input)?;
        let ServiceProviderUpdate::State { sequence, .. } = &update else { return Err("expected state".into()); };
        assert_eq!(sequence.number(), token.parse::<f64>()?);
        let decoded: ServiceProviderUpdate = serde_json::from_str(&serde_json::to_string(&update)?)?;
        assert!(matches!(decoded, ServiceProviderUpdate::State { sequence: actual, .. } if actual == *sequence));
    }
    assert_eq!(ServiceSequence::new(-0.0)?, ServiceSequence::ZERO);
    let boundary = seq(9_007_199_254_740_992);
    assert_eq!(boundary.next().next(), boundary);
    for invalid in [-1.0, 0.5, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        assert!(ServiceSequence::new(invalid).is_err());
    }
    Ok(())
}

#[test]
fn surrogate_service_member_key_and_subscription_identifiers_roundtrip() -> Result<(), ServiceError> {
    let service = JsString::from_utf16(vec![0xd800]);
    let member = JsString::from_utf16(vec![0xdfff]);
    let key = JsString::from_utf16(vec![0xd801]);
    let subscription = JsString::from_utf16(vec![0xdc00]);
    let call = ServiceCall { service_id: service.clone(), member: member.clone(), instance: Some(ServiceInstanceAddress { key: key.clone(), generation: ServiceSequence::new(1e100)? }), args: vec![] };
    let decoded = parse_service_call(maho_chord::json::parse(&serde_json::to_string(&call)?)?)?;
    assert_eq!(decoded.service_id, service); assert_eq!(decoded.member, member);
    let address = decoded.instance.ok_or("missing instance")?;
    assert_eq!(address.key, key); assert_eq!(address.generation.number(), 1e100);
    let control = create_service_subscribe_call(&subscription, &service, ServiceMode::Keyed);
    let control = parse_service_call(maho_chord::json::parse(&serde_json::to_string(&control)?)?)?;
    assert_eq!(decode_service_control_call(&control), Some(ServiceControlCall::Subscribe { subscription_id: subscription.clone(), service_id: service.clone(), mode: ServiceMode::Keyed }));
    assert_eq!(decode_service_control_call(&create_service_unsubscribe_call(&subscription)), Some(ServiceControlCall::Unsubscribe { subscription_id: subscription }));
    let catalogue = maho_chord::json::parse(r#"[{"serviceId":"\ud800","mode":"singleton"},{"serviceId":"\ud801","mode":"keyed"}]"#)?;
    assert_eq!(parse_service_catalogue(catalogue)?.len(), 2);
    assert_eq!(maho_chord::api::define_service::<()>(service.clone(), false)?.id, service);
    let reserved = JsString::from("$chord.").concat(&member);
    assert!(maho_chord::api::define_service::<()>(reserved, false).is_err());
    Ok(())
}

#[test]
fn surrogate_keys_and_values_survive_both_service_wire_codecs() -> Result<(), ServiceError> {
    let value = maho_chord::json::parse(r#"{"\ud800":"\udfff","text":"\ud800"}"#)?;
    let expected = value.clone();
    let mut encoder = create_service_state_encoder(); let mut decoder = create_service_state_decoder();
    let snapshot = ServiceSubscriptionSnapshot { service_id: "test".into(), mode: ServiceMode::Singleton, instances: vec![ServiceInstanceSnapshot { instance: None, members: vec![ServiceMemberSnapshot::State { name: "state".into(), sequence: seq(0), ops: vec![Op::Replace(value)] }] }] };
    let wire = encoder.encode_snapshot(snapshot)?;
    let json = serde_json::to_string(&wire)?;
    let parsed = parse_wire_service_subscription_snapshot(maho_chord::json::parse(&json)?)?;
    let decoded = decoder.decode_snapshot(parsed)?;
    match &decoded.instances[0].members[0] { ServiceMemberSnapshot::State { ops, .. } => assert_eq!(ops, &vec![Op::Replace(expected)]), ServiceMemberSnapshot::Method { .. } => return Err("expected state".into()) }
    let key = JsString::from_utf16(vec![0xd800]);
    let update = ServiceProviderUpdate::State { instance: None, member: "state".into(), sequence: seq(1), ops: vec![Op::Set(vec![key.into()], JsonValue::String(JsString::from_utf16(vec![0xdfff])))] };
    let wire = encoder.encode_update(update)?;
    let json = serde_json::to_string(&wire)?;
    let decoded = decoder.decode_update(parse_wire_service_provider_update(maho_chord::json::parse(&json)?)?)?;
    match decoded { ServiceProviderUpdate::State { ops, .. } => assert!(matches!(&ops[0], Op::Set(path, JsonValue::String(value)) if path == &vec![JsString::from_utf16(vec![0xd800]).into()] && value.units() == vec![0xdfff])), _ => return Err("expected update".into()) }
    Ok(())
}
#[test]
fn control_calls_and_strict_validation() -> Result<(), ServiceError> {
    assert_eq!(
        decode_service_control_call(&create_service_catalogue_call()),
        Some(ServiceControlCall::Catalogue)
    );
    assert_eq!(
        decode_service_control_call(&create_service_subscribe_call(
            &"sub".into(), &"test.models".into(),
            ServiceMode::Singleton
        )),
        Some(ServiceControlCall::Subscribe {
            subscription_id: "sub".into(),
            service_id: "test.models".into(),
            mode: ServiceMode::Singleton
        })
    );
    assert_eq!(
        decode_service_control_call(&create_service_unsubscribe_call(&"sub".into())),
        Some(ServiceControlCall::Unsubscribe {
            subscription_id: "sub".into()
        })
    );
    assert!(
        parse_service_call(json!({"serviceId":"test","member":"call","args":[],"extra":true}))
            .is_err()
    );
    assert!(parse_service_catalogue(json!([{"serviceId":"test","mode":"unknown"}])).is_err());
    assert!(parse_service_catalogue(
        json!([{"serviceId":"test","mode":"singleton"},{"serviceId":"test","mode":"keyed"}])
    )
    .is_err());
    assert!(parse_service_provider_update(
        json!({"type":"state","member":"state","sequence":0,"ops":[]})
    )
    .is_err());
    assert!(parse_wire_service_provider_update(
        json!({"type":"state","member":"state","sequence":1,"ops":[["?",0]]})
    )
    .is_err());
    assert!(parse_service_call(
        json!({"serviceId":"test","instance":{"key":"x","generation":0},"member":"call","args":[]})
    )
    .is_err());
    parse_service_call(
        json!({"serviceId":"test","instance":{"key":"x","generation":2},"member":"call","args":[{"value":true}]}),
    )?;
    Ok(())
}
#[test]
fn reset_validation_and_permutation_wire_support() -> Result<(), ServiceError> {
    let value = json!({"type":"reset","snapshot":{"serviceId":"test","mode":"singleton","instances":[{"members":[{"name":"state","kind":"state","sequence":4,"ops":[["r",{"rows":[1,2]}]]}]}]}});
    parse_service_provider_update(value.clone())?;
    parse_wire_service_provider_update(value)?;
    assert!(parse_service_provider_update(json!({"type":"reset","snapshot":{"serviceId":"test","mode":"singleton","instances":[{"members":[{"name":"state","kind":"state","sequence":4,"ops":[["s",["value"],1]]}]}]}})).is_err());
    let update = json!({"type":"state","member":"state","sequence":5,"ops":[["m",["rows"],[1,0]]]});
    parse_service_provider_update(update.clone())?;
    parse_wire_service_provider_update(update)?;
    let mut encoder = create_service_state_encoder();
    let mut decoder = create_service_state_decoder();
    let mut snapshot = snapshot();
    if let ServiceMemberSnapshot::State { ops, .. } = &mut snapshot.instances[0].members[0] {
        *ops = vec![Op::Replace(json!({"rows":[1,2]}).into())];
    }
    decoder.decode_snapshot(encoder.encode_snapshot(snapshot)?)?;
    for sequence in 1..=2 {
        let update = ServiceProviderUpdate::State {
            instance: None,
            member: "state".into(),
            sequence: seq(sequence),
            ops: vec![Op::Permute(vec!["rows".into()], vec![1, 0])],
        };
        let decoded = decoder.decode_update(encoder.encode_update(update.clone())?)?;
        assert_eq!(
            serde_json::to_value(decoded)?,
            serde_json::to_value(update)?
        );
    }
    Ok(())
}
#[test]
fn state_codecs_retain_dictionaries_and_reset_them_for_replacement() -> Result<(), ServiceError> {
    let mut encoder = create_service_state_encoder();
    let mut decoder = create_service_state_decoder();
    let expected = snapshot();
    let encoded = encoder.encode_snapshot(expected.clone())?;
    let encoded = parse_wire_service_subscription_snapshot(serde_json::to_value(encoded)?)?;
    let decoded = decoder.decode_snapshot(encoded)?;
    assert_eq!(
        serde_json::to_value(decoded)?,
        serde_json::to_value(expected.clone())?
    );
    for sequence in 1..=3 {
        let update = ServiceProviderUpdate::State {
            instance: None,
            member: "state".into(),
            sequence: seq(sequence),
            ops: vec![Op::Set(vec!["revision".into()], json!(sequence).into())],
        };
        let encoded = encoder.encode_update(update.clone())?;
        let decoded = decoder.decode_update(parse_wire_service_provider_update(
            serde_json::to_value(encoded)?,
        )?)?;
        assert_eq!(
            serde_json::to_value(decoded)?,
            serde_json::to_value(update)?
        );
    }
    let reset = ServiceProviderUpdate::Reset {
        snapshot: expected.clone(),
    };
    let decoded = decoder.decode_update(encoder.encode_update(reset.clone())?)?;
    assert_eq!(serde_json::to_value(decoded)?, serde_json::to_value(reset)?);
    decoder.decode_update(encoder.encode_update(ServiceProviderUpdate::Unavailable)?)?;
    assert!(encoder
        .encode_update(ServiceProviderUpdate::State {
            instance: None,
            member: "state".into(),
            sequence: seq(4),
            ops: vec![]
        })
        .is_err());
    Ok(())
}
#[test]
fn keyed_generations_have_independent_codecs_and_close_removes_only_one() -> Result<(), ServiceError>
{
    let mut encoder = create_service_state_encoder();
    let mut decoder = create_service_state_decoder();
    let mut first = snapshot();
    first.mode = ServiceMode::Keyed;
    first.instances[0].instance = Some(ServiceInstanceAddress {
        key: "a".into(),
        generation: seq(1),
    });
    let mut second = first.instances[0].clone();
    second.instance = Some(ServiceInstanceAddress {
        key: "b".into(),
        generation: seq(1),
    });
    first.instances.push(second);
    decoder.decode_snapshot(encoder.encode_snapshot(first)?)?;
    decoder.decode_update(encoder.encode_update(ServiceProviderUpdate::Closed {
        instance: ServiceInstanceAddress {
            key: "a".into(),
            generation: seq(1),
        },
    })?)?;
    let update = ServiceProviderUpdate::State {
        instance: Some(ServiceInstanceAddress {
            key: "b".into(),
            generation: seq(1),
        }),
        member: "state".into(),
        sequence: seq(1),
        ops: vec![Op::Set(vec!["revision".into()], json!(1).into())],
    };
    decoder.decode_update(encoder.encode_update(update)?)?;
    assert!(encoder
        .encode_update(ServiceProviderUpdate::State {
            instance: Some(ServiceInstanceAddress {
                key: "a".into(),
                generation: seq(1)
            }),
            member: "state".into(),
            sequence: seq(1),
            ops: vec![]
        })
        .is_err());
    Ok(())
}

fn seq(value: u64) -> ServiceSequence { ServiceSequence::new(value as f64).expect("small finite integral fixture sequence") }
